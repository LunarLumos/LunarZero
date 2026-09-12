//! Permission engine: rulesets (last match wins), interactive asks that block
//! on a reply, and persisted "always" approvals.

pub mod arity;
pub mod wildcard;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use lz_schema::Event;
use lz_schema::ids::{self, Prefix};
use lz_schema::permission::{Action, Rule, Ruleset};
use lz_schema::session::{PermissionReply, PermissionRequest, ToolRef};
use serde_json::{Map, Value};
use tokio::sync::oneshot;

use crate::bus::Bus;
use crate::storage::{Storage, repo};

#[derive(Debug, Clone, thiserror::Error, PartialEq)]
pub enum PermissionError {
    /// Explicitly denied by a rule; the model is told which rule.
    #[error("{0}")]
    Denied(String),
    /// User rejected the request.
    #[error(
        "Permission denied by the user for this tool call. Do not retry it as-is; choose a different approach or ask."
    )]
    Rejected,
    /// User rejected with feedback for the model.
    #[error("Permission denied by the user, who says: {0}")]
    Corrected(String),
}

/// Flatten the ordered rulesets and find the last matching rule.
pub fn evaluate(permission: &str, pattern: &str, rulesets: &[&Ruleset]) -> Rule {
    let mut result = Rule::new(permission, pattern, Action::Ask);
    for rs in rulesets {
        for rule in rs.iter() {
            if wildcard::matches(permission, &rule.permission) && wildcard::matches(pattern, &rule.pattern) {
                result = rule.clone();
            }
        }
    }
    result
}

/// Which of `tools` are fully disabled (`*` pattern → deny) for a ruleset.
pub fn disabled(tools: &[&str], ruleset: &Ruleset) -> Vec<String> {
    tools
        .iter()
        .filter(|t| evaluate(t, "*", &[ruleset]).action == Action::Deny)
        .map(|t| t.to_string())
        .collect()
}

/// The effective ruleset for a session: agent rules → permission mode → the
/// user's own config again (explicit allow/deny lists beat the mode) → the
/// session's rules. Last match wins.
pub fn effective(
    agent_rules: &Ruleset,
    mode: Option<lz_schema::permission::PermissionMode>,
    user_rules: &Ruleset,
    session_rules: Option<&Ruleset>,
) -> Ruleset {
    let mut ruleset = agent_rules.clone();
    if let Some(mode) = mode {
        ruleset.extend(mode.rules());
        if mode.keeps_user_rules() {
            ruleset.extend(user_rules.iter().cloned());
        }
    }
    if let Some(extra) = session_rules {
        ruleset.extend(extra.iter().cloned());
    }
    ruleset
}

pub fn merge(rulesets: &[&Ruleset]) -> Ruleset {
    rulesets.iter().flat_map(|r| r.iter().cloned()).collect()
}

/// `{"bash": "ask", "edit": {"src/*": "allow"}}` → rules, preserving key order.
pub fn from_config(value: &Value, home: &std::path::Path) -> Ruleset {
    let expand = |s: &str| -> String {
        if s.starts_with("~/") || s == "~" || s.starts_with("$HOME/") {
            crate::paths::expand_home(s, home).to_string_lossy().to_string()
        } else {
            s.to_string()
        }
    };
    let action_of = |v: &Value| -> Option<Action> {
        match v.as_str()? {
            "allow" => Some(Action::Allow),
            "deny" => Some(Action::Deny),
            "ask" => Some(Action::Ask),
            _ => None,
        }
    };
    let mut out = Vec::new();
    match value {
        Value::String(_) => {
            if let Some(a) = action_of(value) {
                out.push(Rule::new("*", "*", a));
            }
        }
        Value::Object(obj) => {
            for (permission, v) in obj {
                match v {
                    Value::String(_) => {
                        if let Some(a) = action_of(v) {
                            out.push(Rule::new(permission, "*", a));
                        }
                    }
                    Value::Object(patterns) => {
                        for (pattern, av) in patterns {
                            if let Some(a) = action_of(av) {
                                out.push(Rule::new(permission, expand(pattern), a));
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
    out
}

pub struct AskInput {
    pub session_id: String,
    pub permission: String,
    pub patterns: Vec<String>,
    /// Patterns that an "always" reply should approve for the rest of the project.
    pub always: Vec<String>,
    pub metadata: Map<String, Value>,
    pub tool: Option<ToolRef>,
    pub ruleset: Ruleset,
    /// Always put the request in front of the user, whatever the rules, the
    /// permission mode or `--auto` say (explicit deny rules still win). Used
    /// for actions the agent picked up from content rather than from the
    /// user — installing third-party code, for one. With nobody to answer
    /// (`--auto`, non-interactive) it is refused.
    pub force: bool,
}

/// What an approval carries back to the tool.
#[derive(Debug, Clone, Default)]
pub struct Grant {
    /// `edit` only: apply just these hunks of the proposed diff.
    pub hunks: Option<Vec<usize>>,
    /// A note from the user for the model (e.g. why hunks were left out).
    pub note: Option<String>,
}

struct Pending {
    request: PermissionRequest,
    tx: oneshot::Sender<Result<Grant, PermissionError>>,
}

pub struct Permissions {
    bus: Bus,
    storage: Storage,
    project_id: String,
    pending: Mutex<BTreeMap<String, Pending>>,
    /// Rules approved via "always" this process (+ loaded from SQLite).
    approved: Mutex<Ruleset>,
    /// `--auto`: approve everything without asking.
    pub auto_approve: bool,
}

impl Permissions {
    pub fn new(bus: Bus, storage: Storage, project_id: &str, auto_approve: bool) -> Arc<Self> {
        let approved = storage
            .with_blocking({
                let pid = project_id.to_string();
                move |c| repo::list_permissions(c, &pid)
            })
            .unwrap_or_default();
        Arc::new(Self {
            bus,
            storage,
            project_id: project_id.to_string(),
            pending: Mutex::new(BTreeMap::new()),
            approved: Mutex::new(approved),
            auto_approve,
        })
    }

    pub fn pending(&self) -> Vec<PermissionRequest> {
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .map(|p| p.request.clone())
            .collect()
    }

    fn approved_ruleset(&self) -> Ruleset {
        self.approved.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Evaluate and, if needed, block until the user replies.
    pub async fn ask(&self, input: AskInput) -> Result<Grant, PermissionError> {
        if self.auto_approve && !input.force {
            return Ok(Grant::default());
        }
        let approved = self.approved_ruleset();
        let rulesets: Vec<&Ruleset> = vec![&input.ruleset, &approved];
        let mut needs_ask = input.force;
        for pattern in &input.patterns {
            let rule = evaluate(&input.permission, pattern, &rulesets);
            match rule.action {
                Action::Deny => {
                    return Err(PermissionError::Denied(format!(
                        "The user has specified a rule to deny this tool call: permission '{}' pattern '{}'. Do not attempt this again.",
                        rule.permission, rule.pattern
                    )));
                }
                Action::Ask => needs_ask = true,
                Action::Allow => {}
            }
        }
        if !needs_ask {
            return Ok(Grant::default());
        }
        if self.auto_approve {
            // forced, but nobody is there to confirm
            return Err(PermissionError::Denied(format!(
                "'{}' needs an explicit confirmation from the user and this session runs unattended (--auto). Ask the user to run it themselves.",
                input.permission
            )));
        }

        let request = PermissionRequest {
            id: ids::ascending(Prefix::Permission),
            session_id: input.session_id.clone(),
            permission: input.permission.clone(),
            patterns: input.patterns.clone(),
            metadata: Value::Object(input.metadata),
            always: input.always.clone(),
            tool: input.tool,
        };
        let (tx, rx) = oneshot::channel();
        {
            let mut p = self.pending.lock().unwrap_or_else(|e| e.into_inner());
            p.insert(
                request.id.clone(),
                Pending {
                    request: request.clone(),
                    tx,
                },
            );
        }
        self.bus.publish(Event::PermissionAsked(request));
        rx.await.unwrap_or(Err(PermissionError::Rejected))
    }

    pub async fn reply(
        &self,
        id: &str,
        reply: PermissionReply,
        message: Option<String>,
        hunks: Option<Vec<usize>>,
    ) -> Result<(), String> {
        let pending = {
            let mut p = self.pending.lock().unwrap_or_else(|e| e.into_inner());
            p.remove(id)
                .ok_or_else(|| format!("no pending permission {id}"))?
        };
        let session_id = pending.request.session_id.clone();
        self.bus.publish(Event::PermissionReplied {
            session_id: session_id.clone(),
            request_id: id.into(),
            reply,
        });
        match reply {
            PermissionReply::Once => {
                let _ = pending.tx.send(Ok(Grant {
                    hunks,
                    note: message.filter(|m| !m.trim().is_empty()),
                }));
            }
            PermissionReply::Always => {
                let rules: Vec<Rule> = pending
                    .request
                    .always
                    .iter()
                    .map(|p| Rule::new(&pending.request.permission, p, Action::Allow))
                    .collect();
                {
                    let mut a = self.approved.lock().unwrap_or_else(|e| e.into_inner());
                    a.extend(rules.iter().cloned());
                }
                let pid = self.project_id.clone();
                let to_save = rules.clone();
                let _ = self
                    .storage
                    .with(move |c| {
                        for r in &to_save {
                            repo::save_permission(c, &ids::ascending(Prefix::Permission), &pid, r)?;
                        }
                        Ok(())
                    })
                    .await;
                let _ = pending.tx.send(Ok(Grant::default()));
                // resolve other pending requests now satisfied
                let approved = self.approved_ruleset();
                let mut satisfied = Vec::new();
                {
                    let p = self.pending.lock().unwrap_or_else(|e| e.into_inner());
                    for (pid, other) in p.iter() {
                        let ok = other.request.patterns.iter().all(|pat| {
                            evaluate(&other.request.permission, pat, &[&approved]).action == Action::Allow
                        });
                        if ok {
                            satisfied.push(pid.clone());
                        }
                    }
                }
                for pid in satisfied {
                    let other = self
                        .pending
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .remove(&pid);
                    if let Some(o) = other {
                        self.bus.publish(Event::PermissionReplied {
                            session_id: o.request.session_id.clone(),
                            request_id: pid,
                            reply: PermissionReply::Always,
                        });
                        let _ = o.tx.send(Ok(Grant::default()));
                    }
                }
            }
            PermissionReply::Reject => {
                let err = match message.filter(|m| !m.trim().is_empty()) {
                    Some(m) => PermissionError::Corrected(m),
                    None => PermissionError::Rejected,
                };
                let _ = pending.tx.send(Err(err));
                // reject everything else pending for this session
                let others: Vec<Pending> = {
                    let mut p = self.pending.lock().unwrap_or_else(|e| e.into_inner());
                    let ids: Vec<String> = p
                        .iter()
                        .filter(|(_, o)| o.request.session_id == session_id)
                        .map(|(k, _)| k.clone())
                        .collect();
                    ids.into_iter().filter_map(|k| p.remove(&k)).collect()
                };
                for o in others {
                    self.bus.publish(Event::PermissionReplied {
                        session_id: o.request.session_id.clone(),
                        request_id: o.request.id.clone(),
                        reply: PermissionReply::Reject,
                    });
                    let _ = o.tx.send(Err(PermissionError::Rejected));
                }
            }
        }
        Ok(())
    }

    /// Fail every pending request for a session (used on abort).
    pub fn cancel_session(&self, session_id: &str) {
        let others: Vec<Pending> = {
            let mut p = self.pending.lock().unwrap_or_else(|e| e.into_inner());
            let ids: Vec<String> = p
                .iter()
                .filter(|(_, o)| o.request.session_id == session_id)
                .map(|(k, _)| k.clone())
                .collect();
            ids.into_iter().filter_map(|k| p.remove(&k)).collect()
        };
        for o in others {
            let _ = o.tx.send(Err(PermissionError::Rejected));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn last_match_wins_and_default_ask() {
        let rs = vec![
            Rule::new("bash", "*", Action::Allow),
            Rule::new("bash", "rm *", Action::Deny),
            Rule::new("bash", "rm -rf /tmp/*", Action::Allow),
        ];
        assert_eq!(evaluate("bash", "ls -la", &[&rs]).action, Action::Allow);
        assert_eq!(evaluate("bash", "rm foo", &[&rs]).action, Action::Deny);
        assert_eq!(evaluate("bash", "rm -rf /tmp/x", &[&rs]).action, Action::Allow);
        assert_eq!(evaluate("edit", "x", &[&rs]).action, Action::Ask);
    }

    #[test]
    fn deny_beats_everything_and_specific_after_general() {
        let rs = from_config(
            &serde_json::json!({
                "bash": { "*": "allow", "rm *": "deny", "git *": "ask", "git status": "allow" }
            }),
            std::path::Path::new("/"),
        );
        assert_eq!(evaluate("bash", "ls -la", &[&rs]).action, Action::Allow);
        assert_eq!(evaluate("bash", "rm -rf x", &[&rs]).action, Action::Deny);
        assert_eq!(evaluate("bash", "git push", &[&rs]).action, Action::Ask);
        assert_eq!(evaluate("bash", "git status", &[&rs]).action, Action::Allow);
        // a later ruleset overrides an earlier one, rule by rule
        let later = from_config(
            &serde_json::json!({ "bash": { "rm *": "allow" } }),
            std::path::Path::new("/"),
        );
        assert_eq!(evaluate("bash", "rm -rf x", &[&rs, &later]).action, Action::Allow);
        // an unrelated permission never matches
        assert_eq!(evaluate("edit", "rm -rf x", &[&rs]).action, Action::Ask);
    }

    #[test]
    fn modes_tighten_defaults_but_user_rules_win() {
        use lz_schema::permission::PermissionMode;
        let defaults = from_config(&serde_json::json!({ "*": "allow" }), std::path::Path::new("/"));
        let user = from_config(
            &serde_json::json!({ "bash": { "git status": "allow", "rm *": "deny" } }),
            std::path::Path::new("/"),
        );
        let agent = merge(&[&defaults, &user]);
        let eval = |mode: Option<PermissionMode>, perm: &str, pat: &str| {
            let rs = effective(&agent, mode, &user, None);
            evaluate(perm, pat, &[&rs]).action
        };
        // no mode: legacy behaviour, everything allowed except user denies
        assert_eq!(eval(None, "edit", "src/a.rs"), Action::Allow);
        assert_eq!(eval(None, "bash", "rm -rf x"), Action::Deny);
        // manual: edits and commands ask, but the user's allow-list still wins
        assert_eq!(
            eval(Some(PermissionMode::Manual), "edit", "src/a.rs"),
            Action::Ask
        );
        assert_eq!(
            eval(Some(PermissionMode::Manual), "bash", "cargo test"),
            Action::Ask
        );
        assert_eq!(
            eval(Some(PermissionMode::Manual), "bash", "git status"),
            Action::Allow
        );
        assert_eq!(
            eval(Some(PermissionMode::Manual), "bash", "rm -rf x"),
            Action::Deny
        );
        // accept edits: edits through, commands still ask
        assert_eq!(
            eval(Some(PermissionMode::AcceptEdits), "edit", "src/a.rs"),
            Action::Allow
        );
        assert_eq!(
            eval(Some(PermissionMode::AcceptEdits), "bash", "cargo test"),
            Action::Ask
        );
        // auto: nothing asks — but an explicit user deny is kept by the session rules layer
        assert_eq!(
            eval(Some(PermissionMode::Auto), "bash", "cargo test"),
            Action::Allow
        );
        assert_eq!(
            eval(Some(PermissionMode::Auto), "edit", "src/a.rs"),
            Action::Allow
        );
        // plan: edits denied outright, commands ask
        assert_eq!(eval(Some(PermissionMode::Plan), "edit", "src/a.rs"), Action::Deny);
        assert_eq!(
            eval(Some(PermissionMode::Plan), "bash", "cargo test"),
            Action::Ask
        );
        // session rules are the last word
        let session = from_config(&serde_json::json!({ "edit": "deny" }), std::path::Path::new("/"));
        let rs = effective(&agent, Some(PermissionMode::Auto), &user, Some(&session));
        assert_eq!(evaluate("edit", "src/a.rs", &[&rs]).action, Action::Deny);
    }

    #[tokio::test]
    async fn forced_ask_is_refused_when_unattended() {
        // --auto approves everything… except a forced request, which needs a human
        let storage = crate::storage::Storage::open_in_memory().unwrap();
        let bus = crate::bus::Bus::new(None);
        let perms = Permissions::new(bus, storage, "p", true);
        let ok = perms
            .ask(AskInput {
                session_id: "s".into(),
                permission: "bash".into(),
                patterns: vec!["ls".into()],
                always: vec![],
                metadata: Default::default(),
                tool: None,
                ruleset: Vec::new(),
                force: false,
            })
            .await;
        assert!(ok.is_ok());
        let forced = perms
            .ask(AskInput {
                session_id: "s".into(),
                permission: "install".into(),
                patterns: vec!["https://example.com/x".into()],
                always: vec![],
                metadata: Default::default(),
                tool: None,
                ruleset: Vec::new(),
                force: true,
            })
            .await;
        assert!(matches!(forced, Err(PermissionError::Denied(_))), "{forced:?}");
    }

    #[test]
    fn config_conversion_keeps_order() {
        let v: Value = serde_json::from_str(
            r#"{"bash": {"git *": "allow", "rm *": "deny"}, "edit": "ask", "*": "allow"}"#,
        )
        .unwrap();
        let rs = from_config(&v, std::path::Path::new("/home/u"));
        assert_eq!(rs.len(), 4);
        assert_eq!(rs[0], Rule::new("bash", "git *", Action::Allow));
        assert_eq!(rs[3], Rule::new("*", "*", Action::Allow));
        let rs2 = from_config(&Value::String("deny".into()), std::path::Path::new("/"));
        assert_eq!(rs2[0], Rule::new("*", "*", Action::Deny));
    }
}
