//! `EngineApi` — the transport-agnostic contract between the engine and its
//! clients (TUI, `run` command, and a future HTTP client).

use std::collections::BTreeMap;

use async_trait::async_trait;
use futures::stream::BoxStream;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::Config;
use crate::permission::Ruleset;
use crate::session::*;

pub type ApiResult<T> = Result<T, ApiError>;

#[derive(Debug, Clone, thiserror::Error, Serialize, Deserialize, PartialEq)]
#[serde(tag = "name", content = "data")]
pub enum ApiError {
    #[error("not found: {message}")]
    NotFound { message: String },
    #[error("session is busy")]
    Busy,
    #[error("invalid: {message}")]
    Invalid { message: String },
    #[error("provider auth: {message}")]
    ProviderAuth { message: String },
    #[error("{message}")]
    Internal { message: String },
}

impl ApiError {
    pub fn internal(e: impl std::fmt::Display) -> Self {
        ApiError::Internal {
            message: e.to_string(),
        }
    }
    pub fn not_found(e: impl std::fmt::Display) -> Self {
        ApiError::NotFound {
            message: e.to_string(),
        }
    }
    pub fn invalid(e: impl std::fmt::Display) -> Self {
        ApiError::Invalid {
            message: e.to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelInfo {
    pub id: String,
    #[serde(rename = "providerID")]
    pub provider_id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub family: Option<String>,
    pub reasoning: bool,
    pub tool_call: bool,
    pub attachment: bool,
    pub temperature: bool,
    pub cost: ModelCost,
    pub limit: ModelLimit,
    #[serde(default)]
    pub variants: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// Present for free-pool members (scores/limits) and the virtual `lunar` models.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pool: Option<PoolInfo>,
}

/// Free-pool metadata for a model: `quality` and `speed` are 0–100 scores
/// (higher is better), the rest are the provider's free-tier caps.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PoolInfo {
    pub quality: u32,
    pub speed: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rpm: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rpd: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tpm: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tpd: Option<u64>,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq)]
pub struct ModelCost {
    pub input: f64,
    pub output: f64,
    #[serde(default)]
    pub cache_read: f64,
    #[serde(default)]
    pub cache_write: f64,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq)]
pub struct ModelLimit {
    pub context: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<f64>,
    pub output: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProviderInfo {
    pub id: String,
    pub name: String,
    /// `env` | `config` | `auth` | `catalog`
    pub source: String,
    pub connected: bool,
    pub models: Vec<ModelInfo>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ProvidersResponse {
    pub providers: Vec<ProviderInfo>,
    /// providerID → default modelID
    pub default: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentInfo {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub mode: crate::config::AgentMode,
    pub hidden: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    pub permission: Ruleset,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub steps: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f64>,
    pub builtin: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CommandInfo {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default)]
    pub subtask: bool,
    pub template: String,
    /// `builtin` | `config` | `skill` | `mcp`
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SkillInfo {
    pub name: String,
    pub description: String,
    pub location: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LspStatus {
    pub id: String,
    pub name: String,
    pub root: String,
    /// `connected` | `error`
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum McpStatus {
    Connected,
    Disabled,
    Failed { error: String },
    NeedsAuth,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PathInfo {
    pub cwd: String,
    pub root: String,
    pub worktree: String,
    pub directory: String,
    pub config: String,
    pub data: String,
    pub state: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct SessionQuery {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub search: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
    /// Only root sessions (no parent).
    #[serde(default)]
    pub roots: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct CreateSession {
    #[serde(rename = "parentID", default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<SessionId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission: Option<Ruleset>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct SessionPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived: Option<Option<f64>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission: Option<Ruleset>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct MessagesQuery {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<MessageId>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CommandRequest {
    pub command: String,
    #[serde(default)]
    pub arguments: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ShellRequest {
    pub command: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PermissionReplyRequest {
    pub reply: PermissionReply,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GrepMatch {
    pub path: String,
    pub line: u64,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FileStatus {
    pub path: String,
    /// `added` | `deleted` | `modified`
    pub status: String,
    pub additions: u64,
    pub deletions: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum AuthInfo {
    Api {
        key: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        metadata: Option<Value>,
    },
    OAuth {
        refresh: String,
        access: String,
        expires: u64,
    },
}

#[async_trait]
pub trait EngineApi: Send + Sync + 'static {
    // bootstrap
    async fn config(&self) -> ApiResult<Config>;
    async fn providers(&self) -> ApiResult<ProvidersResponse>;
    async fn agents(&self) -> ApiResult<Vec<AgentInfo>>;
    async fn commands(&self) -> ApiResult<Vec<CommandInfo>>;
    async fn skills(&self) -> ApiResult<Vec<SkillInfo>>;
    async fn lsp_status(&self) -> ApiResult<Vec<LspStatus>>;
    async fn mcp_status(&self) -> ApiResult<BTreeMap<String, McpStatus>>;
    async fn path(&self) -> ApiResult<PathInfo>;

    // sessions
    async fn list_sessions(&self, q: SessionQuery) -> ApiResult<Vec<SessionInfo>>;
    async fn session_status(&self) -> ApiResult<BTreeMap<SessionId, SessionStatus>>;
    async fn get_session(&self, id: &str) -> ApiResult<SessionInfo>;
    async fn create_session(&self, opts: CreateSession) -> ApiResult<SessionInfo>;
    async fn update_session(&self, id: &str, patch: SessionPatch) -> ApiResult<SessionInfo>;
    async fn delete_session(&self, id: &str) -> ApiResult<()>;
    async fn children(&self, id: &str) -> ApiResult<Vec<SessionInfo>>;
    async fn messages(&self, id: &str, q: MessagesQuery) -> ApiResult<Vec<MessageWithParts>>;
    async fn message(&self, id: &str, message_id: &str) -> ApiResult<MessageWithParts>;
    async fn todos(&self, id: &str) -> ApiResult<Vec<Todo>>;
    async fn diff(&self, id: &str) -> ApiResult<Vec<FileDiff>>;

    /// Submit a prompt and wait for the assistant turn(s) to finish.
    async fn prompt(&self, id: &str, req: PromptRequest) -> ApiResult<MessageWithParts>;
    /// Submit a prompt and return immediately.
    async fn prompt_async(&self, id: &str, req: PromptRequest) -> ApiResult<()>;
    async fn command(&self, id: &str, req: CommandRequest) -> ApiResult<()>;
    async fn shell(&self, id: &str, req: ShellRequest) -> ApiResult<()>;
    async fn abort(&self, id: &str) -> ApiResult<()>;
    async fn summarize(&self, id: &str, model: Option<ModelRef>) -> ApiResult<()>;
    async fn fork(&self, id: &str, at: Option<MessageId>) -> ApiResult<SessionInfo>;
    async fn revert(&self, id: &str, message_id: &str, part_id: Option<PartId>) -> ApiResult<SessionInfo>;
    async fn unrevert(&self, id: &str) -> ApiResult<SessionInfo>;
    async fn init(&self, id: &str, model: Option<ModelRef>) -> ApiResult<()>;

    // interaction
    async fn pending_permissions(&self) -> ApiResult<Vec<PermissionRequest>>;
    async fn reply_permission(&self, id: &str, reply: PermissionReplyRequest) -> ApiResult<()>;
    async fn pending_questions(&self) -> ApiResult<Vec<QuestionRequest>>;
    async fn reply_question(&self, id: &str, answers: Vec<Vec<String>>) -> ApiResult<()>;
    async fn reject_question(&self, id: &str) -> ApiResult<()>;

    // files / auth / mcp
    async fn find_files(&self, query: &str, limit: usize) -> ApiResult<Vec<String>>;
    async fn grep(&self, pattern: &str, limit: usize) -> ApiResult<Vec<GrepMatch>>;
    async fn file_status(&self) -> ApiResult<Vec<FileStatus>>;
    async fn read_file(&self, path: &str) -> ApiResult<String>;
    async fn set_auth(&self, provider: &str, auth: AuthInfo) -> ApiResult<()>;
    async fn remove_auth(&self, provider: &str) -> ApiResult<()>;
    /// Install skills from a git source (`owner/repo`, GitHub URL, optional sub-path).
    async fn install_skill(&self, source: &str, global: bool) -> ApiResult<Vec<SkillInfo>>;
    async fn remove_skill(&self, name: &str) -> ApiResult<()>;
    async fn mcp_connect(&self, name: &str) -> ApiResult<()>;
    async fn mcp_disconnect(&self, name: &str) -> ApiResult<()>;

    // events
    fn subscribe(&self) -> BoxStream<'static, crate::event::Event>;
}
