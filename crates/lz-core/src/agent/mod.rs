//! Agent definitions: built-ins (`build`, `plan`, `general`, `explore`,
//! hidden `compaction`/`title`/`summary`) plus user overrides from config and
//! markdown files..

use std::collections::BTreeMap;
use std::path::Path;

use lz_schema::api::AgentInfo;
use lz_schema::config::AgentMode;
use lz_schema::permission::Ruleset;
use lz_schema::session::ModelRef;
use serde_json::{Map, Value, json};

use crate::paths::Paths;
use crate::permission::{from_config, merge};

pub const PROMPT_EXPLORE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../assets/prompts/agents/explore.md"
));
pub const PROMPT_COMPACTION: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../assets/prompts/agents/compaction.md"
));
pub const PROMPT_TITLE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../assets/prompts/agents/title.md"
));
pub const PROMPT_SUMMARY: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../assets/prompts/agents/summary.md"
));

#[derive(Debug, Clone)]
pub struct Agent {
    pub name: String,
    pub description: Option<String>,
    pub mode: AgentMode,
    pub hidden: bool,
    pub builtin: bool,
    pub color: Option<String>,
    pub model: Option<ModelRef>,
    pub variant: Option<String>,
    pub prompt: Option<String>,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub steps: Option<u32>,
    pub options: Map<String, Value>,
    pub permission: Ruleset,
}

impl Agent {
    pub fn to_info(&self) -> AgentInfo {
        AgentInfo {
            name: self.name.clone(),
            description: self.description.clone(),
            mode: self.mode,
            hidden: self.hidden,
            color: self.color.clone(),
            model: self.model.clone(),
            prompt: self.prompt.clone(),
            permission: self.permission.clone(),
            steps: self.steps,
            temperature: self.temperature,
            top_p: self.top_p,
            builtin: self.builtin,
        }
    }
}

pub struct Agents {
    pub agents: BTreeMap<String, Agent>,
}

fn parse_model(s: &str) -> Option<ModelRef> {
    let (p, m) = s.split_once('/')?;
    Some(ModelRef {
        provider_id: p.into(),
        model_id: m.into(),
        variant: None,
    })
}

pub fn build(raw_config: &Map<String, Value>, paths: &Paths, worktree: &Path) -> Agents {
    let home = &paths.home;
    let tool_output_glob = format!("{}/*", paths.tool_output().display());
    let tmp_glob = format!("{}/*", std::env::temp_dir().join("lunarzero").display());
    let plans_glob = format!("{}/*", paths.plans().display());
    let plans_md = format!("{}/*.md", paths.plans().display());
    let plans_rel = pathdiff(worktree, &paths.plans()).map(|p| format!("{p}/*.md"));

    let defaults = from_config(
        &json!({
            "*": "allow",
            "doom_loop": "ask",
            "external_directory": { "*": "ask", tool_output_glob.clone(): "allow", tmp_glob.clone(): "allow" },
            "question": "deny",
            "plan_enter": "deny",
            "plan_exit": "deny",
            "read": { "*": "allow", "*.env": "ask", "*.env.*": "ask", "*.env.example": "allow" }
        }),
        home,
    );
    let user = raw_config
        .get("permission")
        .map(|v| from_config(v, home))
        .unwrap_or_default();

    let mk =
        |name: &str, mode: AgentMode, description: Option<&str>, prompt: Option<&str>, extra: Value| Agent {
            name: name.into(),
            description: description.map(str::to_string),
            mode,
            hidden: false,
            builtin: true,
            color: None,
            model: None,
            variant: None,
            prompt: prompt.map(str::to_string),
            temperature: None,
            top_p: None,
            steps: None,
            options: Map::new(),
            permission: merge(&[&defaults, &from_config(&extra, home), &user]),
        };

    let mut agents: BTreeMap<String, Agent> = BTreeMap::new();
    agents.insert(
        "build".into(),
        mk(
            "build",
            AgentMode::Primary,
            Some("The default agent. Executes tools based on configured permissions."),
            None,
            json!({ "question": "allow", "plan_enter": "allow" }),
        ),
    );
    let mut plan_edit = json!({ "*": "deny", ".opencode/plans/*.md": "allow", ".lunarzero/plans/*.md": "allow", plans_md.clone(): "allow" });
    if let Some(rel) = &plans_rel {
        plan_edit[rel.clone()] = json!("allow");
    }
    agents.insert(
        "plan".into(),
        mk(
            "plan",
            AgentMode::Primary,
            Some("Plan mode. Disallows all edit tools."),
            None,
            json!({
                "question": "allow",
                "plan_exit": "allow",
                "task": { "general": "deny" },
                "external_directory": { plans_glob.clone(): "allow" },
                "edit": plan_edit
            }),
        ),
    );
    agents.insert(
        "general".into(),
        mk(
            "general",
            AgentMode::Subagent,
            Some("Multi-step work with all tools; good for parallel independent tasks."),
            None,
            json!({ "todowrite": "deny" }),
        ),
    );
    agents.insert(
        "explore".into(),
        mk(
            "explore",
            AgentMode::Subagent,
            Some("Read-only codebase research: find files, search code, answer questions; say how thorough."),
            Some(PROMPT_EXPLORE),
            json!({
                "*": "deny", "grep": "allow", "glob": "allow", "list": "allow", "bash": "allow",
                "webfetch": "allow", "websearch": "allow", "read": "allow",
                "external_directory": { "*": "ask", tool_output_glob.clone(): "allow", tmp_glob.clone(): "allow" }
            }),
        ),
    );
    for (name, prompt, temp) in [
        ("compaction", PROMPT_COMPACTION, None),
        ("title", PROMPT_TITLE, Some(0.5)),
        ("summary", PROMPT_SUMMARY, None),
    ] {
        let mut a = mk(
            name,
            AgentMode::Primary,
            None,
            Some(prompt),
            json!({ "*": "deny" }),
        );
        a.hidden = true;
        a.temperature = temp;
        agents.insert(name.into(), a);
    }

    // user overrides / custom agents
    if let Some(Value::Object(cfg_agents)) = raw_config.get("agent") {
        for (key, value) in cfg_agents {
            let Value::Object(v) = value else { continue };
            if v.get("disable").and_then(Value::as_bool) == Some(true) {
                agents.remove(key);
                continue;
            }
            let item = agents.entry(key.clone()).or_insert_with(|| Agent {
                name: key.clone(),
                description: None,
                mode: AgentMode::All,
                hidden: false,
                builtin: false,
                color: None,
                model: None,
                variant: None,
                prompt: None,
                temperature: None,
                top_p: None,
                steps: None,
                options: Map::new(),
                permission: merge(&[&defaults, &user]),
            });
            if let Some(m) = v.get("model").and_then(Value::as_str) {
                item.model = parse_model(m);
            }
            if let Some(s) = v.get("variant").and_then(Value::as_str) {
                item.variant = Some(s.into());
            }
            if let Some(s) = v.get("prompt").and_then(Value::as_str) {
                item.prompt = Some(s.into());
            }
            if let Some(s) = v.get("description").and_then(Value::as_str) {
                item.description = Some(s.into());
            }
            if let Some(n) = v.get("temperature").and_then(Value::as_f64) {
                item.temperature = Some(n);
            }
            if let Some(n) = v.get("top_p").and_then(Value::as_f64) {
                item.top_p = Some(n);
            }
            if let Some(s) = v.get("mode").and_then(Value::as_str) {
                item.mode = match s {
                    "subagent" => AgentMode::Subagent,
                    "primary" => AgentMode::Primary,
                    _ => AgentMode::All,
                };
            }
            if let Some(s) = v.get("color").and_then(Value::as_str) {
                item.color = Some(s.into());
            }
            if let Some(b) = v.get("hidden").and_then(Value::as_bool) {
                item.hidden = b;
            }
            if let Some(s) = v.get("name").and_then(Value::as_str) {
                item.name = s.into();
            }
            if let Some(n) = v
                .get("steps")
                .or_else(|| v.get("maxSteps"))
                .and_then(Value::as_u64)
            {
                item.steps = Some(n as u32);
            }
            if let Some(Value::Object(o)) = v.get("options") {
                for (k, val) in o {
                    item.options.insert(k.clone(), val.clone());
                }
            }
            // `tools: {write: false}` → permission edit: deny
            let mut perm = Map::new();
            if let Some(Value::Object(tools)) = v.get("tools") {
                for (tool, enabled) in tools {
                    let action = if enabled.as_bool().unwrap_or(false) {
                        "allow"
                    } else {
                        "deny"
                    };
                    let k = if matches!(tool.as_str(), "write" | "edit" | "patch") {
                        "edit"
                    } else {
                        tool
                    };
                    perm.insert(k.to_string(), json!(action));
                }
            }
            if let Some(p) = v.get("permission") {
                match p {
                    Value::Object(o) => {
                        for (k, val) in o {
                            perm.insert(k.clone(), val.clone());
                        }
                    }
                    Value::String(_) => {
                        perm.insert("*".into(), p.clone());
                    }
                    _ => {}
                }
            }
            if !perm.is_empty() {
                let extra = from_config(&Value::Object(perm), home);
                item.permission = merge(&[&item.permission, &extra]);
            }
        }
    }
    Agents { agents }
}

fn pathdiff(base: &Path, target: &Path) -> Option<String> {
    target.strip_prefix(base).ok().map(|p| p.display().to_string())
}

impl Agents {
    pub fn get(&self, name: &str) -> Option<&Agent> {
        self.agents.get(name)
    }
    pub fn list(&self) -> Vec<&Agent> {
        self.agents.values().collect()
    }
    pub fn default_agent<'a>(&'a self, configured: Option<&str>) -> &'a Agent {
        configured
            .and_then(|n| self.agents.get(n))
            .filter(|a| a.mode != AgentMode::Subagent)
            .or_else(|| self.agents.get("build"))
            .unwrap_or_else(|| self.agents.values().next().expect("at least one agent"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lz_schema::permission::Action;

    fn paths() -> Paths {
        Paths {
            home: "/h".into(),
            config: "/h/.config/lunarzero".into(),
            data: "/h/.local/share/lunarzero".into(),
            cache: "/h/.cache/lunarzero".into(),
            state: "/h/.local/state/lunarzero".into(),
            legacy_config: "/h/.config/opencode".into(),
            legacy_data: "/h/.local/share/opencode".into(),
        }
    }

    #[test]
    fn builtins_and_overrides() {
        let raw: Map<String, Value> = serde_json::from_str(
            r#"{ "permission": { "bash": "ask" }, "agent": { "plan": { "model": "openai/gpt-4o" }, "reviewer": { "mode": "subagent", "tools": { "write": false }, "prompt": "review" } } }"#,
        )
        .unwrap();
        let a = build(&raw, &paths(), Path::new("/proj"));
        let plan = a.get("plan").unwrap();
        assert_eq!(plan.model.as_ref().unwrap().model_id, "gpt-4o");
        assert_eq!(
            crate::permission::evaluate("edit", "src/x.rs", &[&plan.permission]).action,
            Action::Deny
        );
        assert_eq!(
            crate::permission::evaluate("bash", "ls", &[&plan.permission]).action,
            Action::Ask
        );
        let r = a.get("reviewer").unwrap();
        assert_eq!(r.mode, AgentMode::Subagent);
        assert_eq!(
            crate::permission::evaluate("edit", "x", &[&r.permission]).action,
            Action::Deny
        );
        assert_eq!(
            crate::permission::evaluate("read", "x", &[&r.permission]).action,
            Action::Allow
        );
        let build_agent = a.get("build").unwrap();
        assert_eq!(
            crate::permission::evaluate("question", "*", &[&build_agent.permission]).action,
            Action::Allow
        );
        assert_eq!(
            crate::permission::evaluate("read", ".env", &[&build_agent.permission]).action,
            Action::Ask
        );
    }
}
