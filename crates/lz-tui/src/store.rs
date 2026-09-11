//! Client-side mirror of engine state, kept current by applying bus events.

use std::collections::{BTreeMap, HashMap, HashSet};

use lz_schema::Event;
use lz_schema::api::*;
use lz_schema::config::Config;
use lz_schema::session::*;

#[derive(Default)]
pub struct Store {
    pub sessions: BTreeMap<String, SessionInfo>,
    /// session id → messages sorted by id
    pub messages: HashMap<String, Vec<Message>>,
    /// message id → parts sorted by id
    pub parts: HashMap<String, Vec<Part>>,
    pub status: HashMap<String, SessionStatus>,
    pub todos: HashMap<String, Vec<Todo>>,
    pub diffs: HashMap<String, Vec<FileDiff>>,
    pub permissions: Vec<PermissionRequest>,
    pub questions: Vec<QuestionRequest>,
    pub lsp: Vec<LspStatus>,
    pub mcp: BTreeMap<String, McpStatus>,
    pub providers: ProvidersResponse,
    pub agents: Vec<AgentInfo>,
    pub commands: Vec<CommandInfo>,
    pub skills: Vec<SkillInfo>,
    pub config: Config,
    pub path: Option<PathInfo>,
    /// Parts changed since the last render (invalidates render caches).
    pub touched: HashSet<String>,
    pub last_error: Option<(String, MessageError)>,
    /// Sessions whose history is fully loaded.
    pub loaded: HashSet<String>,
    /// session → (provider, model, reason) last chosen by the free-pool router.
    pub routed: HashMap<String, (String, String, String)>,
}

impl Store {
    pub fn messages_of(&self, session_id: &str) -> &[Message] {
        self.messages.get(session_id).map(Vec::as_slice).unwrap_or(&[])
    }

    pub fn parts_of(&self, message_id: &str) -> &[Part] {
        self.parts.get(message_id).map(Vec::as_slice).unwrap_or(&[])
    }

    pub fn status_of(&self, session_id: &str) -> SessionStatus {
        self.status.get(session_id).cloned().unwrap_or_default()
    }

    pub fn is_busy(&self, session_id: &str) -> bool {
        !matches!(self.status_of(session_id), SessionStatus::Idle)
    }

    pub fn load_messages(&mut self, session_id: &str, msgs: Vec<MessageWithParts>) {
        let mut list = Vec::with_capacity(msgs.len());
        for m in msgs {
            for p in &m.parts {
                self.touched.insert(p.id.clone());
            }
            self.parts.insert(m.info.id().to_string(), m.parts);
            list.push(m.info);
        }
        list.sort_by(|a, b| a.id().cmp(b.id()));
        self.messages.insert(session_id.into(), list);
        self.loaded.insert(session_id.into());
    }

    fn upsert_message(&mut self, info: Message) {
        let list = self.messages.entry(info.session_id().to_string()).or_default();
        match list.binary_search_by(|m| m.id().cmp(info.id())) {
            Ok(i) => list[i] = info,
            Err(i) => list.insert(i, info),
        }
    }

    fn upsert_part(&mut self, part: Part) {
        self.touched.insert(part.id.clone());
        let list = self.parts.entry(part.message_id.clone()).or_default();
        match list.binary_search_by(|p| p.id.cmp(&part.id)) {
            Ok(i) => list[i] = part,
            Err(i) => list.insert(i, part),
        }
    }

    pub fn apply(&mut self, event: Event) {
        match event {
            Event::SessionCreated { info, .. } | Event::SessionUpdated { info, .. } => {
                self.sessions.insert(info.id.clone(), info);
            }
            Event::SessionDeleted { session_id, .. } => {
                self.sessions.remove(&session_id);
                if let Some(msgs) = self.messages.remove(&session_id) {
                    for m in msgs {
                        self.parts.remove(m.id());
                    }
                }
                self.status.remove(&session_id);
                self.loaded.remove(&session_id);
            }
            Event::SessionStatus { session_id, status } => {
                if matches!(status, SessionStatus::Idle) {
                    self.status.remove(&session_id);
                } else {
                    self.status.insert(session_id, status);
                }
            }
            Event::SessionError { session_id, error } => {
                self.last_error = Some((session_id.unwrap_or_default(), error));
            }
            Event::SessionDiff { session_id, diff } => {
                self.diffs.insert(session_id, diff);
            }
            Event::SessionCompacted { .. } => {}
            Event::ModelRouted {
                session_id,
                provider_id,
                model_id,
                reason,
                ..
            } => {
                self.routed.insert(session_id, (provider_id, model_id, reason));
            }
            Event::MessageUpdated { info, .. } => self.upsert_message(info),
            Event::MessageRemoved {
                session_id,
                message_id,
            } => {
                if let Some(list) = self.messages.get_mut(&session_id) {
                    list.retain(|m| m.id() != message_id);
                }
                self.parts.remove(&message_id);
            }
            Event::PartUpdated { part, .. } => self.upsert_part(part),
            Event::PartRemoved {
                message_id, part_id, ..
            } => {
                if let Some(list) = self.parts.get_mut(&message_id) {
                    list.retain(|p| p.id != part_id);
                }
                self.touched.insert(part_id);
            }
            Event::PartDelta {
                message_id,
                part_id,
                field,
                delta,
                ..
            } => {
                if let Some(list) = self.parts.get_mut(&message_id)
                    && let Ok(i) = list.binary_search_by(|p| p.id.cmp(&part_id))
                {
                    match &mut list[i].kind {
                        PartKind::Text { text, .. } | PartKind::Reasoning { text, .. } if field == "text" => {
                            text.push_str(&delta)
                        }
                        _ => {}
                    }
                    self.touched.insert(part_id);
                }
            }
            Event::PermissionAsked(req) => {
                if !self.permissions.iter().any(|p| p.id == req.id) {
                    self.permissions.push(req);
                }
            }
            Event::PermissionReplied { request_id, .. } => self.permissions.retain(|p| p.id != request_id),
            Event::QuestionAsked(req) => {
                if !self.questions.iter().any(|q| q.id == req.id) {
                    self.questions.push(req);
                }
            }
            Event::QuestionReplied { request_id, .. } | Event::QuestionRejected { request_id, .. } => {
                self.questions.retain(|q| q.id != request_id)
            }
            Event::TodoUpdated { session_id, todos } => {
                self.todos.insert(session_id, todos);
            }
            Event::McpStatus { name, status } => {
                self.mcp.insert(name, status);
            }
            Event::FileEdited { .. }
            | Event::LspUpdated {}
            | Event::ConfigUpdated {}
            | Event::ServerConnected {}
            | Event::ServerHeartbeat {} => {}
        }
    }

    /// Pending permission for a session (oldest first).
    pub fn permission_for(&self, session_id: &str) -> Option<&PermissionRequest> {
        self.permissions.iter().find(|p| p.session_id == session_id)
    }
    pub fn question_for(&self, session_id: &str) -> Option<&QuestionRequest> {
        self.questions.iter().find(|q| q.session_id == session_id)
    }

    /// (context tokens after the last completed step, total cost) for a session,
    /// derived from assistant messages.
    pub fn usage(&self, session_id: &str) -> (f64, f64) {
        let mut cost = 0.0;
        let mut context = 0.0;
        for m in self.messages_of(session_id) {
            if let Message::Assistant(a) = m {
                cost += a.cost;
                let t = a.tokens.effective_total();
                if t > 0.0 {
                    context = t;
                }
            }
        }
        if let Some(s) = self.sessions.get(session_id)
            && let Some(c) = s.cost
        {
            cost = cost.max(c);
        }
        (context, cost)
    }

    pub fn model_info(&self, provider: &str, model: &str) -> Option<&ModelInfo> {
        self.providers
            .providers
            .iter()
            .find(|p| p.id == provider)?
            .models
            .iter()
            .find(|m| m.id == model)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn part(id: &str, msg: &str, text: &str) -> Part {
        Part {
            id: id.into(),
            session_id: "ses_1".into(),
            message_id: msg.into(),
            kind: PartKind::Text {
                text: text.into(),
                synthetic: false,
                ignored: false,
                time: None,
                metadata: None,
            },
        }
    }

    fn assistant(id: &str) -> Message {
        Message::Assistant(AssistantMessage {
            id: id.into(),
            session_id: "ses_1".into(),
            time: AssistantTime {
                created: 1,
                completed: None,
            },
            error: None,
            parent_id: "msg_u".into(),
            model_id: "m".into(),
            provider_id: "p".into(),
            mode: "build".into(),
            agent: "build".into(),
            path: MessagePath {
                cwd: "/".into(),
                root: "/".into(),
            },
            summary: None,
            cost: 0.5,
            tokens: Tokens {
                total: None,
                input: 10.0,
                output: 5.0,
                reasoning: 0.0,
                cache: CacheTokens::default(),
            },
            structured: None,
            variant: None,
            finish: None,
        })
    }

    #[test]
    fn applies_message_and_part_events_in_order() {
        let mut s = Store::default();
        s.apply(Event::MessageUpdated {
            session_id: "ses_1".into(),
            info: assistant("msg_a"),
        });
        s.apply(Event::PartUpdated {
            session_id: "ses_1".into(),
            part: part("prt_2", "msg_a", "world"),
        });
        s.apply(Event::PartUpdated {
            session_id: "ses_1".into(),
            part: part("prt_1", "msg_a", "hello "),
        });
        s.apply(Event::PartDelta {
            session_id: "ses_1".into(),
            message_id: "msg_a".into(),
            part_id: "prt_2".into(),
            field: "text".into(),
            delta: "!".into(),
        });
        let parts = s.parts_of("msg_a");
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].id, "prt_1");
        assert!(matches!(&parts[1].kind, PartKind::Text { text, .. } if text == "world!"));
        assert!(s.touched.contains("prt_2"));
        assert_eq!(s.usage("ses_1"), (15.0, 0.5));
        s.apply(Event::PartRemoved {
            session_id: "ses_1".into(),
            message_id: "msg_a".into(),
            part_id: "prt_1".into(),
        });
        assert_eq!(s.parts_of("msg_a").len(), 1);
        s.apply(Event::MessageRemoved {
            session_id: "ses_1".into(),
            message_id: "msg_a".into(),
        });
        assert!(s.messages_of("ses_1").is_empty());
        assert!(s.parts_of("msg_a").is_empty());
    }

    #[test]
    fn permission_and_status_lifecycle() {
        let mut s = Store::default();
        let req = PermissionRequest {
            id: "per_1".into(),
            session_id: "ses_1".into(),
            permission: "bash".into(),
            patterns: vec!["rm *".into()],
            metadata: serde_json::Value::Null,
            always: vec![],
            tool: None,
        };
        s.apply(Event::PermissionAsked(req.clone()));
        s.apply(Event::PermissionAsked(req));
        assert_eq!(s.permissions.len(), 1);
        assert!(s.permission_for("ses_1").is_some());
        s.apply(Event::SessionStatus {
            session_id: "ses_1".into(),
            status: SessionStatus::Busy,
        });
        assert!(s.is_busy("ses_1"));
        s.apply(Event::PermissionReplied {
            session_id: "ses_1".into(),
            request_id: "per_1".into(),
            reply: PermissionReply::Once,
        });
        assert!(s.permission_for("ses_1").is_none());
        s.apply(Event::SessionStatus {
            session_id: "ses_1".into(),
            status: SessionStatus::Idle,
        });
        assert!(!s.is_busy("ses_1"));
    }
}
