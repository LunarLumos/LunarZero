//! Session / message / part data model. Field names and tag values are
//! stable so the JSON on disk stays readable by other tools.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::permission::{PermissionMode, Ruleset};

pub type SessionId = String;
pub type MessageId = String;
pub type PartId = String;

fn is_false(b: &bool) -> bool {
    !*b
}

// ───────────────────────────── file diff ─────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum FileDiffStatus {
    Added,
    Deleted,
    Modified,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct FileDiff {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub patch: Option<String>,
    pub additions: f64,
    pub deletions: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<FileDiffStatus>,
}

// ───────────────────────────── tokens / cost ─────────────────────────────

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq)]
pub struct CacheTokens {
    pub read: f64,
    pub write: f64,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq)]
pub struct Tokens {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total: Option<f64>,
    pub input: f64,
    pub output: f64,
    pub reasoning: f64,
    pub cache: CacheTokens,
}

impl Tokens {
    /// Total tokens in context after this step (`total` when reported, else input+output+cache).
    pub fn effective_total(&self) -> f64 {
        self.total
            .unwrap_or(self.input + self.output + self.cache.read + self.cache.write)
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq)]
pub struct SessionTokens {
    pub input: f64,
    pub output: f64,
    pub reasoning: f64,
    pub cache: CacheTokens,
}

// ───────────────────────────── session ─────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionSummary {
    pub additions: f64,
    pub deletions: f64,
    pub files: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diffs: Option<Vec<FileDiff>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionRevert {
    #[serde(rename = "messageID")]
    pub message_id: MessageId,
    #[serde(rename = "partID", default, skip_serializing_if = "Option::is_none")]
    pub part_id: Option<PartId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionModel {
    pub id: String,
    #[serde(rename = "providerID")]
    pub provider_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variant: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct SessionTime {
    pub created: u64,
    pub updated: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compacting: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionInfo {
    pub id: SessionId,
    pub slug: String,
    #[serde(rename = "projectID")]
    pub project_id: String,
    pub directory: String,
    #[serde(rename = "parentID", default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<SessionId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<SessionSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<SessionTokens>,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<SessionModel>,
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<BTreeMap<String, Value>>,
    pub time: SessionTime,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission: Option<Ruleset>,
    /// Live permission mode (manual / accept-edits / auto / plan).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<PermissionMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revert: Option<SessionRevert>,
}

// ───────────────────────────── messages ─────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OutputFormat {
    Text,
    JsonSchema {
        schema: Value,
        #[serde(rename = "retryCount", default = "default_retry_count")]
        retry_count: u32,
    },
}

fn default_retry_count() -> u32 {
    2
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelRef {
    #[serde(rename = "providerID")]
    pub provider_id: String,
    #[serde(rename = "modelID")]
    pub model_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variant: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UserSummary {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    pub diffs: Vec<FileDiff>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UserTime {
    pub created: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UserMessage {
    pub id: MessageId,
    #[serde(rename = "sessionID")]
    pub session_id: SessionId,
    pub time: UserTime,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<OutputFormat>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<UserSummary>,
    pub agent: String,
    pub model: ModelRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<BTreeMap<String, bool>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AssistantTime {
    pub created: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MessagePath {
    pub cwd: String,
    pub root: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ApiErrorData {
    pub message: String,
    #[serde(rename = "statusCode", default, skip_serializing_if = "Option::is_none")]
    pub status_code: Option<u32>,
    #[serde(rename = "isRetryable")]
    pub is_retryable: bool,
    #[serde(rename = "responseHeaders", default, skip_serializing_if = "Option::is_none")]
    pub response_headers: Option<BTreeMap<String, String>>,
    #[serde(rename = "responseBody", default, skip_serializing_if = "Option::is_none")]
    pub response_body: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<BTreeMap<String, String>>,
}

/// Assistant-message error. Serialized as `{ "name": "...", "data": {...} }`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "name", content = "data")]
pub enum MessageError {
    #[serde(rename = "ProviderAuthError")]
    ProviderAuth {
        #[serde(rename = "providerID")]
        provider_id: String,
        message: String,
    },
    #[serde(rename = "UnknownError")]
    Unknown {
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        r#ref: Option<String>,
    },
    #[serde(rename = "MessageOutputLengthError")]
    OutputLength {},
    #[serde(rename = "MessageAbortedError")]
    Aborted { message: String },
    #[serde(rename = "StructuredOutputError")]
    StructuredOutput { message: String, retries: u32 },
    #[serde(rename = "ContextOverflowError")]
    ContextOverflow {
        message: String,
        #[serde(rename = "responseBody", default, skip_serializing_if = "Option::is_none")]
        response_body: Option<String>,
    },
    #[serde(rename = "ContentFilterError")]
    ContentFilter { message: String },
    #[serde(rename = "APIError")]
    Api(ApiErrorData),
}

impl MessageError {
    pub fn message(&self) -> String {
        match self {
            MessageError::ProviderAuth { message, .. }
            | MessageError::Unknown { message, .. }
            | MessageError::Aborted { message }
            | MessageError::StructuredOutput { message, .. }
            | MessageError::ContextOverflow { message, .. }
            | MessageError::ContentFilter { message } => message.clone(),
            MessageError::OutputLength {} => "Output length exceeded".into(),
            MessageError::Api(d) => d.message.clone(),
        }
    }

    /// One short line for the UI: provider JSON bodies are reduced to their
    /// `error.message`, whitespace collapsed, capped at `max` chars.
    pub fn summary(&self, max: usize) -> String {
        let raw = self.message();
        let text = json_error_message(&raw).unwrap_or(raw);
        let mut one: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
        // drop trailing "For more information…" boilerplate
        for marker in [
            " For more information",
            " To monitor",
            " Learn more",
            " See https://",
        ] {
            if let Some(i) = one.find(marker) {
                one.truncate(i);
            }
        }
        let status = match self {
            MessageError::Api(d) => d.status_code.map(|c| format!("HTTP {c}: ")).unwrap_or_default(),
            _ => String::new(),
        };
        let mut s = format!("{status}{one}");
        if s.chars().count() > max {
            s = s.chars().take(max.saturating_sub(1)).collect::<String>() + "…";
        }
        s
    }
}

/// `{"error":{"message":…}}`, `[{"error":…}]`, `{"message":…}` → message.
pub fn json_error_message(text: &str) -> Option<String> {
    let t = text.trim();
    if !(t.starts_with('{') || t.starts_with('[')) {
        return None;
    }
    let v: Value = serde_json::from_str(t).ok()?;
    let v = if let Value::Array(a) = &v {
        a.first().cloned()?
    } else {
        v
    };
    v.pointer("/error/message")
        .or_else(|| v.get("message"))
        .or_else(|| v.pointer("/error"))
        .and_then(|m| {
            m.as_str()
                .map(str::to_string)
                .or_else(|| m.get("message").and_then(Value::as_str).map(str::to_string))
        })
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AssistantMessage {
    pub id: MessageId,
    #[serde(rename = "sessionID")]
    pub session_id: SessionId,
    pub time: AssistantTime,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<MessageError>,
    #[serde(rename = "parentID")]
    pub parent_id: MessageId,
    #[serde(rename = "modelID")]
    pub model_id: String,
    #[serde(rename = "providerID")]
    pub provider_id: String,
    pub mode: String,
    pub agent: String,
    pub path: MessagePath,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<bool>,
    pub cost: f64,
    pub tokens: Tokens,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structured: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variant: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finish: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "role", rename_all = "lowercase")]
pub enum Message {
    User(UserMessage),
    Assistant(AssistantMessage),
}

impl Message {
    pub fn id(&self) -> &str {
        match self {
            Message::User(m) => &m.id,
            Message::Assistant(m) => &m.id,
        }
    }
    pub fn session_id(&self) -> &str {
        match self {
            Message::User(m) => &m.session_id,
            Message::Assistant(m) => &m.session_id,
        }
    }
    pub fn created(&self) -> u64 {
        match self {
            Message::User(m) => m.time.created,
            Message::Assistant(m) => m.time.created,
        }
    }
    pub fn as_user(&self) -> Option<&UserMessage> {
        match self {
            Message::User(m) => Some(m),
            _ => None,
        }
    }
    pub fn as_assistant(&self) -> Option<&AssistantMessage> {
        match self {
            Message::Assistant(m) => Some(m),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MessageWithParts {
    pub info: Message,
    pub parts: Vec<Part>,
}

// ───────────────────────────── parts ─────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PartTime {
    pub start: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SourceText {
    pub value: String,
    pub start: f64,
    pub end: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Position {
    pub line: u32,
    pub character: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Range {
    pub start: Position,
    pub end: Position,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum FilePartSource {
    File {
        text: SourceText,
        path: String,
    },
    Symbol {
        text: SourceText,
        path: String,
        range: Range,
        name: String,
        kind: u32,
    },
    Resource {
        text: SourceText,
        #[serde(rename = "clientName")]
        client_name: String,
        uri: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FilePart {
    pub id: PartId,
    #[serde(rename = "sessionID")]
    pub session_id: SessionId,
    #[serde(rename = "messageID")]
    pub message_id: MessageId,
    pub mime: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<FilePartSource>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolTimeRunning {
    pub start: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolTimeCompleted {
    pub start: u64,
    pub end: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compacted: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolTimeError {
    pub start: u64,
    pub end: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum ToolState {
    Pending {
        input: Value,
        raw: String,
    },
    Running {
        input: Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        metadata: Option<Value>,
        time: ToolTimeRunning,
    },
    Completed {
        input: Value,
        output: String,
        title: String,
        metadata: Value,
        time: ToolTimeCompleted,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        attachments: Option<Vec<FilePart>>,
    },
    Error {
        input: Value,
        error: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        metadata: Option<Value>,
        time: ToolTimeError,
    },
}

impl ToolState {
    pub fn input(&self) -> &Value {
        match self {
            ToolState::Pending { input, .. }
            | ToolState::Running { input, .. }
            | ToolState::Completed { input, .. }
            | ToolState::Error { input, .. } => input,
        }
    }
    pub fn status(&self) -> &'static str {
        match self {
            ToolState::Pending { .. } => "pending",
            ToolState::Running { .. } => "running",
            ToolState::Completed { .. } => "completed",
            ToolState::Error { .. } => "error",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentSource {
    pub value: String,
    pub start: u64,
    pub end: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SubtaskModel {
    #[serde(rename = "providerID")]
    pub provider_id: String,
    #[serde(rename = "modelID")]
    pub model_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RetryTime {
    pub created: u64,
}

/// Every part shares `id`, `sessionID`, `messageID`; the variant carries the
/// remaining fields. Tag value = `type`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum PartKind {
    Text {
        text: String,
        #[serde(default, skip_serializing_if = "is_false")]
        synthetic: bool,
        #[serde(default, skip_serializing_if = "is_false")]
        ignored: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        time: Option<PartTime>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        metadata: Option<Value>,
    },
    Reasoning {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        metadata: Option<Value>,
        time: PartTime,
    },
    File {
        mime: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        filename: Option<String>,
        url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source: Option<FilePartSource>,
    },
    Tool {
        #[serde(rename = "callID")]
        call_id: String,
        tool: String,
        state: ToolState,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        metadata: Option<Value>,
    },
    StepStart {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        snapshot: Option<String>,
    },
    StepFinish {
        reason: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        snapshot: Option<String>,
        cost: f64,
        tokens: Tokens,
    },
    Snapshot {
        snapshot: String,
    },
    Patch {
        hash: String,
        files: Vec<String>,
    },
    Agent {
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source: Option<AgentSource>,
    },
    Subtask {
        prompt: String,
        description: String,
        agent: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<SubtaskModel>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        command: Option<String>,
    },
    Retry {
        attempt: u32,
        error: MessageError,
        time: RetryTime,
    },
    Compaction {
        auto: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        overflow: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tail_start_id: Option<MessageId>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Part {
    pub id: PartId,
    #[serde(rename = "sessionID")]
    pub session_id: SessionId,
    #[serde(rename = "messageID")]
    pub message_id: MessageId,
    #[serde(flatten)]
    pub kind: PartKind,
}

impl Part {
    pub fn type_name(&self) -> &'static str {
        match self.kind {
            PartKind::Text { .. } => "text",
            PartKind::Reasoning { .. } => "reasoning",
            PartKind::File { .. } => "file",
            PartKind::Tool { .. } => "tool",
            PartKind::StepStart { .. } => "step-start",
            PartKind::StepFinish { .. } => "step-finish",
            PartKind::Snapshot { .. } => "snapshot",
            PartKind::Patch { .. } => "patch",
            PartKind::Agent { .. } => "agent",
            PartKind::Subtask { .. } => "subtask",
            PartKind::Retry { .. } => "retry",
            PartKind::Compaction { .. } => "compaction",
        }
    }

    pub fn as_file(&self) -> Option<FilePart> {
        match &self.kind {
            PartKind::File {
                mime,
                filename,
                url,
                source,
            } => Some(FilePart {
                id: self.id.clone(),
                session_id: self.session_id.clone(),
                message_id: self.message_id.clone(),
                mime: mime.clone(),
                filename: filename.clone(),
                url: url.clone(),
                source: source.clone(),
            }),
            _ => None,
        }
    }
}

// ───────────────────────────── prompt input ─────────────────────────────

/// Parts a client can submit with a prompt (subset of `Part` without ids).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum PartInput {
    Text {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<PartId>,
        text: String,
        #[serde(default, skip_serializing_if = "is_false")]
        synthetic: bool,
        #[serde(default, skip_serializing_if = "is_false")]
        ignored: bool,
    },
    File {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<PartId>,
        mime: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        filename: Option<String>,
        url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source: Option<FilePartSource>,
    },
    Agent {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<PartId>,
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source: Option<AgentSource>,
    },
    Subtask {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<PartId>,
        prompt: String,
        description: String,
        agent: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<SubtaskModel>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        command: Option<String>,
    },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PromptRequest {
    #[serde(rename = "messageID", default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<MessageId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variant: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<BTreeMap<String, bool>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<OutputFormat>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub no_reply: bool,
    /// Permission mode to run this (and later) turns under.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<PermissionMode>,
    pub parts: Vec<PartInput>,
}

// ───────────────────────────── status / todo ─────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "lowercase")]
#[derive(Default)]
pub enum SessionStatus {
    #[default]
    Idle,
    Busy,
    Retry {
        attempt: u32,
        message: String,
        next: u64,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Todo {
    pub content: String,
    pub status: String,
    pub priority: String,
}

// ───────────────────────────── permission / question requests ─────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolRef {
    #[serde(rename = "messageID")]
    pub message_id: MessageId,
    #[serde(rename = "callID")]
    pub call_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PermissionRequest {
    pub id: String,
    #[serde(rename = "sessionID")]
    pub session_id: SessionId,
    pub permission: String,
    pub patterns: Vec<String>,
    pub metadata: Value,
    pub always: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<ToolRef>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PermissionReply {
    Once,
    Always,
    Reject,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct QuestionOption {
    pub label: String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct QuestionInfo {
    pub question: String,
    pub header: String,
    pub options: Vec<QuestionOption>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub multiple: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct QuestionRequest {
    pub id: String,
    #[serde(rename = "sessionID")]
    pub session_id: SessionId,
    pub questions: Vec<QuestionInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<ToolRef>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn tool_part_roundtrip_keeps_wire_shape() {
        let json = serde_json::json!({
            "id": "prt_1", "sessionID": "ses_1", "messageID": "msg_1",
            "type": "tool", "callID": "call_1", "tool": "bash",
            "state": { "status": "completed", "input": {"command": "ls"}, "output": "a\nb",
                       "title": "ls", "metadata": {}, "time": {"start": 1, "end": 2} }
        });
        let part: Part = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(part.type_name(), "tool");
        assert_eq!(serde_json::to_value(&part).unwrap(), json);
    }

    #[test]
    fn assistant_error_tagging() {
        let err = MessageError::Api(ApiErrorData {
            message: "boom".into(),
            status_code: Some(500),
            is_retryable: true,
            response_headers: None,
            response_body: None,
            metadata: None,
        });
        let v = serde_json::to_value(&err).unwrap();
        assert_eq!(v["name"], "APIError");
        assert_eq!(v["data"]["statusCode"], 500);
    }

    #[test]
    fn message_role_tag() {
        let json = serde_json::json!({
            "id": "msg_1", "sessionID": "ses_1", "role": "user", "time": {"created": 1},
            "agent": "build", "model": {"providerID": "openai", "modelID": "gpt-4o"}
        });
        let m: Message = serde_json::from_value(json).unwrap();
        assert!(matches!(m, Message::User(_)));
    }
}
