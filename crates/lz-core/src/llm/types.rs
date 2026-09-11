//! Provider-neutral request / event types.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SystemBlock {
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum ContentPart {
    Text {
        text: String,
    },
    Media {
        /// e.g. `image/png`
        mime: String,
        /// base64 data (no `data:` prefix)
        data: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        filename: Option<String>,
    },
    Reasoning {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        metadata: Option<Value>,
    },
    ToolCall {
        id: String,
        name: String,
        input: Value,
    },
    ToolResult {
        id: String,
        name: String,
        result: ToolResultContent,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum ToolResultContent {
    Text { text: String },
    Error { text: String },
    Content { items: Vec<ToolContentItem> },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum ToolContentItem {
    Text {
        text: String,
    },
    File {
        mime: String,
        data: String,
        name: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "role", rename_all = "lowercase")]
pub enum LlmMessage {
    System { content: Vec<ContentPart> },
    User { content: Vec<ContentPart> },
    Assistant { content: Vec<ContentPart> },
    Tool { content: Vec<ContentPart> },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ToolChoice {
    Auto,
    None,
    Required,
    Tool { name: String },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Generation {
    pub max_tokens: Option<u64>,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub top_k: Option<u64>,
    pub stop: Option<Vec<String>>,
    pub seed: Option<u64>,
    pub frequency_penalty: Option<f64>,
    pub presence_penalty: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResponseFormat {
    Text,
    JsonSchema {
        name: String,
        schema: Value,
        strict: bool,
    },
}

/// Neutral request. `provider_options` carries protocol-specific knobs
/// (e.g. `{"openai": {"reasoningEffort": "high"}}`).
#[derive(Debug, Clone, Default)]
pub struct LlmRequest {
    pub model_id: String,
    pub system: Vec<SystemBlock>,
    pub messages: Vec<LlmMessage>,
    pub tools: Vec<ToolDef>,
    pub tool_choice: Option<ToolChoice>,
    pub generation: Generation,
    pub response_format: Option<ResponseFormat>,
    pub provider_options: serde_json::Map<String, Value>,
    pub headers: Vec<(String, String)>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum FinishReason {
    Stop,
    Length,
    ContentFilter,
    ToolCalls,
    Error,
    Other,
    Unknown,
}

impl FinishReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            FinishReason::Stop => "stop",
            FinishReason::Length => "length",
            FinishReason::ContentFilter => "content-filter",
            FinishReason::ToolCalls => "tool-calls",
            FinishReason::Error => "error",
            FinishReason::Other => "other",
            FinishReason::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq)]
pub struct Usage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub non_cached_input_tokens: Option<u64>,
    pub cache_read_input_tokens: Option<u64>,
    pub cache_write_input_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum LlmEvent {
    StepStart {
        index: u32,
    },
    TextStart {
        id: String,
    },
    TextDelta {
        id: String,
        text: String,
    },
    TextEnd {
        id: String,
    },
    ReasoningStart {
        id: String,
    },
    ReasoningDelta {
        id: String,
        text: String,
    },
    ReasoningEnd {
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        metadata: Option<Value>,
    },
    ToolInputStart {
        id: String,
        name: String,
    },
    ToolInputDelta {
        id: String,
        name: String,
        text: String,
    },
    ToolInputEnd {
        id: String,
        name: String,
    },
    ToolCall {
        id: String,
        name: String,
        input: Value,
    },
    StepFinish {
        index: u32,
        reason: FinishReason,
        usage: Option<Usage>,
    },
    Finish {
        reason: FinishReason,
        usage: Option<Usage>,
    },
}

#[derive(Debug, Clone, thiserror::Error, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum LlmError {
    #[error("invalid request: {message}")]
    InvalidRequest { message: String },
    #[error("authentication failed: {message}")]
    Authentication { message: String },
    #[error("rate limited: {message}")]
    RateLimited {
        message: String,
        retry_after_ms: Option<u64>,
    },
    #[error("context overflow: {message}")]
    ContextOverflow { message: String },
    #[error("content policy: {message}")]
    ContentPolicy { message: String },
    #[error("provider error {status}: {message}")]
    Provider {
        status: u16,
        message: String,
        retry_after_ms: Option<u64>,
        headers: std::collections::BTreeMap<String, String>,
        body: Option<String>,
    },
    #[error("invalid provider output: {message}")]
    InvalidOutput { message: String },
    #[error("network error: {message}")]
    Network { message: String },
    #[error("request timed out: {message}")]
    Timeout { message: String },
    #[error("aborted")]
    Aborted,
}

impl LlmError {
    pub fn retryable(&self) -> bool {
        match self {
            LlmError::RateLimited { .. } | LlmError::Network { .. } | LlmError::Timeout { .. } => true,
            LlmError::Provider { status, .. } => *status >= 500 || *status == 408 || *status == 409,
            _ => false,
        }
    }
    pub fn status_code(&self) -> Option<u16> {
        match self {
            LlmError::Provider { status, .. } => Some(*status),
            LlmError::RateLimited { .. } => Some(429),
            LlmError::Authentication { .. } => Some(401),
            _ => None,
        }
    }
    pub fn retry_after_ms(&self) -> Option<u64> {
        match self {
            LlmError::RateLimited { retry_after_ms, .. } | LlmError::Provider { retry_after_ms, .. } => {
                *retry_after_ms
            }
            _ => None,
        }
    }
}
