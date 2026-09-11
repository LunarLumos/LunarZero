//! Tool trait, execution context, and the registry that decides which tools a
//! given agent/model may see.

pub mod builtins;
pub mod registry;
pub mod truncate;

use std::borrow::Cow;
use std::sync::Arc;

use async_trait::async_trait;
use lz_schema::session::{FilePart, MessageWithParts};
use serde_json::{Map, Value};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::agent::Agent;
use crate::permission::{AskInput, PermissionError};

pub use registry::ToolRegistry;

/// Streamed progress from a running tool (title / metadata for the UI).
#[derive(Debug, Clone)]
pub struct ToolProgress {
    pub title: Option<String>,
    pub metadata: Option<Value>,
}

#[derive(Debug, Clone, Default)]
pub struct ToolResult {
    pub title: String,
    pub output: String,
    pub metadata: Value,
    pub attachments: Vec<FilePart>,
}

impl ToolResult {
    pub fn text(title: impl Into<String>, output: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            output: output.into(),
            metadata: Value::Object(Map::new()),
            attachments: Vec::new(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    /// Bad arguments; the message goes back to the model.
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    Permission(#[from] PermissionError),
    #[error("Tool execution aborted")]
    Aborted,
    #[error("{0}")]
    Other(String),
}

impl ToolError {
    pub fn other(e: impl std::fmt::Display) -> Self {
        ToolError::Other(e.to_string())
    }
    /// True when the user rejected the call (loop should stop after this step).
    pub fn is_rejection(&self) -> bool {
        matches!(
            self,
            ToolError::Permission(PermissionError::Rejected | PermissionError::Corrected(_))
        )
    }
}

/// Everything a tool may need while executing.
pub struct ToolCtx {
    pub session_id: String,
    pub message_id: String,
    pub call_id: String,
    pub agent: Arc<Agent>,
    pub cancel: CancellationToken,
    pub engine: Arc<crate::engine::Engine>,
    pub progress: mpsc::UnboundedSender<ToolProgress>,
    /// History at the time of the call (for lazy instruction attachment etc.).
    pub messages: Arc<Vec<MessageWithParts>>,
    /// The user @-mentioned the subagent explicitly → skip `task` permission.
    pub bypass_agent_check: bool,
}

impl ToolCtx {
    pub fn report(&self, title: Option<String>, metadata: Option<Value>) {
        let _ = self.progress.send(ToolProgress { title, metadata });
    }

    /// Ask for permission using the agent's ruleset merged with the session's.
    pub async fn ask(
        &self,
        permission: &str,
        patterns: Vec<String>,
        always: Vec<String>,
        metadata: Map<String, Value>,
    ) -> Result<(), PermissionError> {
        let mut ruleset = self.agent.permission.clone();
        if let Ok(session) = self.engine.sessions.get(&self.session_id).await
            && let Some(extra) = session.permission
        {
            ruleset.extend(extra);
        }
        self.engine
            .permissions
            .ask(AskInput {
                session_id: self.session_id.clone(),
                permission: permission.into(),
                patterns,
                always,
                metadata,
                tool: Some(lz_schema::session::ToolRef {
                    message_id: self.message_id.clone(),
                    call_id: self.call_id.clone(),
                }),
                ruleset,
            })
            .await
    }

    pub fn directory(&self) -> &std::path::Path {
        &self.engine.directory
    }
    pub fn worktree(&self) -> &std::path::Path {
        &self.engine.project.worktree
    }
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn id(&self) -> &'static str;
    fn description(&self) -> Cow<'static, str>;
    fn parameters(&self) -> Value;
    async fn execute(&self, ctx: ToolCtx, args: Value) -> Result<ToolResult, ToolError>;
}

/// Decode JSON args into a typed struct, turning serde errors into
/// model-readable `Invalid` messages.
pub fn parse_args<T: serde::de::DeserializeOwned>(args: Value) -> Result<T, ToolError> {
    serde_json::from_value(args).map_err(|e| ToolError::Invalid(format!("Invalid arguments: {e}")))
}

/// Load the shared tool description prompt.
#[macro_export]
macro_rules! tool_description {
    ($name:literal) => {
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../assets/prompts/tools/",
            $name,
            ".md"
        ))
    };
}
