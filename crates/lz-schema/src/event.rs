//! Bus events. Serialized as `{ "type": "...", "properties": {...} }` — the
//! a stable wire shape for clients.

use serde::{Deserialize, Serialize};

use crate::session::*;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", content = "properties")]
pub enum Event {
    #[serde(rename = "session.created")]
    SessionCreated {
        #[serde(rename = "sessionID")]
        session_id: SessionId,
        info: SessionInfo,
    },
    #[serde(rename = "session.updated")]
    SessionUpdated {
        #[serde(rename = "sessionID")]
        session_id: SessionId,
        info: SessionInfo,
    },
    #[serde(rename = "session.deleted")]
    SessionDeleted {
        #[serde(rename = "sessionID")]
        session_id: SessionId,
        info: SessionInfo,
    },
    #[serde(rename = "session.status")]
    SessionStatus {
        #[serde(rename = "sessionID")]
        session_id: SessionId,
        status: SessionStatus,
    },
    #[serde(rename = "session.error")]
    SessionError {
        #[serde(rename = "sessionID", default, skip_serializing_if = "Option::is_none")]
        session_id: Option<SessionId>,
        error: MessageError,
    },
    #[serde(rename = "session.diff")]
    SessionDiff {
        #[serde(rename = "sessionID")]
        session_id: SessionId,
        diff: Vec<FileDiff>,
    },
    /// The free-pool router picked (or switched to) a concrete model.
    #[serde(rename = "model.routed")]
    ModelRouted {
        #[serde(rename = "sessionID")]
        session_id: SessionId,
        #[serde(rename = "messageID")]
        message_id: MessageId,
        #[serde(rename = "providerID")]
        provider_id: String,
        #[serde(rename = "modelID")]
        model_id: String,
        /// Why: `auto` (initial pick) or the failure that caused a switch.
        reason: String,
    },
    #[serde(rename = "session.compacted")]
    SessionCompacted {
        #[serde(rename = "sessionID")]
        session_id: SessionId,
    },
    #[serde(rename = "message.updated")]
    MessageUpdated {
        #[serde(rename = "sessionID")]
        session_id: SessionId,
        info: Message,
    },
    #[serde(rename = "message.removed")]
    MessageRemoved {
        #[serde(rename = "sessionID")]
        session_id: SessionId,
        #[serde(rename = "messageID")]
        message_id: MessageId,
    },
    #[serde(rename = "message.part.updated")]
    PartUpdated {
        #[serde(rename = "sessionID")]
        session_id: SessionId,
        part: Part,
    },
    #[serde(rename = "message.part.removed")]
    PartRemoved {
        #[serde(rename = "sessionID")]
        session_id: SessionId,
        #[serde(rename = "messageID")]
        message_id: MessageId,
        #[serde(rename = "partID")]
        part_id: PartId,
    },
    #[serde(rename = "message.part.delta")]
    PartDelta {
        #[serde(rename = "sessionID")]
        session_id: SessionId,
        #[serde(rename = "messageID")]
        message_id: MessageId,
        #[serde(rename = "partID")]
        part_id: PartId,
        field: String,
        delta: String,
    },
    #[serde(rename = "permission.asked")]
    PermissionAsked(PermissionRequest),
    #[serde(rename = "permission.replied")]
    PermissionReplied {
        #[serde(rename = "sessionID")]
        session_id: SessionId,
        #[serde(rename = "requestID")]
        request_id: String,
        reply: PermissionReply,
    },
    #[serde(rename = "question.asked")]
    QuestionAsked(QuestionRequest),
    #[serde(rename = "question.replied")]
    QuestionReplied {
        #[serde(rename = "sessionID")]
        session_id: SessionId,
        #[serde(rename = "requestID")]
        request_id: String,
        answers: Vec<Vec<String>>,
    },
    #[serde(rename = "question.rejected")]
    QuestionRejected {
        #[serde(rename = "sessionID")]
        session_id: SessionId,
        #[serde(rename = "requestID")]
        request_id: String,
    },
    #[serde(rename = "todo.updated")]
    TodoUpdated {
        #[serde(rename = "sessionID")]
        session_id: SessionId,
        todos: Vec<Todo>,
    },
    #[serde(rename = "file.edited")]
    FileEdited { file: String },
    #[serde(rename = "lsp.updated")]
    LspUpdated {},
    #[serde(rename = "mcp.status")]
    McpStatus {
        name: String,
        status: crate::api::McpStatus,
    },
    #[serde(rename = "config.updated")]
    ConfigUpdated {},
    #[serde(rename = "server.connected")]
    ServerConnected {},
    #[serde(rename = "server.heartbeat")]
    ServerHeartbeat {},
}

impl Event {
    pub fn session_id(&self) -> Option<&str> {
        match self {
            Event::SessionCreated { session_id, .. }
            | Event::SessionUpdated { session_id, .. }
            | Event::SessionDeleted { session_id, .. }
            | Event::SessionStatus { session_id, .. }
            | Event::SessionDiff { session_id, .. }
            | Event::SessionCompacted { session_id }
            | Event::ModelRouted { session_id, .. }
            | Event::MessageUpdated { session_id, .. }
            | Event::MessageRemoved { session_id, .. }
            | Event::PartUpdated { session_id, .. }
            | Event::PartRemoved { session_id, .. }
            | Event::PartDelta { session_id, .. }
            | Event::PermissionReplied { session_id, .. }
            | Event::QuestionReplied { session_id, .. }
            | Event::QuestionRejected { session_id, .. }
            | Event::TodoUpdated { session_id, .. } => Some(session_id),
            Event::SessionError { session_id, .. } => session_id.as_deref(),
            Event::PermissionAsked(r) => Some(&r.session_id),
            Event::QuestionAsked(r) => Some(&r.session_id),
            _ => None,
        }
    }

    pub fn type_name(&self) -> &'static str {
        match self {
            Event::SessionCreated { .. } => "session.created",
            Event::SessionUpdated { .. } => "session.updated",
            Event::SessionDeleted { .. } => "session.deleted",
            Event::SessionStatus { .. } => "session.status",
            Event::SessionError { .. } => "session.error",
            Event::SessionDiff { .. } => "session.diff",
            Event::SessionCompacted { .. } => "session.compacted",
            Event::ModelRouted { .. } => "model.routed",
            Event::MessageUpdated { .. } => "message.updated",
            Event::MessageRemoved { .. } => "message.removed",
            Event::PartUpdated { .. } => "message.part.updated",
            Event::PartRemoved { .. } => "message.part.removed",
            Event::PartDelta { .. } => "message.part.delta",
            Event::PermissionAsked(_) => "permission.asked",
            Event::PermissionReplied { .. } => "permission.replied",
            Event::QuestionAsked(_) => "question.asked",
            Event::QuestionReplied { .. } => "question.replied",
            Event::QuestionRejected { .. } => "question.rejected",
            Event::TodoUpdated { .. } => "todo.updated",
            Event::FileEdited { .. } => "file.edited",
            Event::LspUpdated {} => "lsp.updated",
            Event::McpStatus { .. } => "mcp.status",
            Event::ConfigUpdated {} => "config.updated",
            Event::ServerConnected {} => "server.connected",
            Event::ServerHeartbeat {} => "server.heartbeat",
        }
    }

    /// Whether this event mutates durable state (and should be appended to the event log).
    pub fn is_durable(&self) -> bool {
        matches!(
            self,
            Event::SessionCreated { .. }
                | Event::SessionUpdated { .. }
                | Event::SessionDeleted { .. }
                | Event::MessageUpdated { .. }
                | Event::MessageRemoved { .. }
                | Event::PartUpdated { .. }
                | Event::PartRemoved { .. }
        )
    }
}
