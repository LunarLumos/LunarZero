//! `Protocol`: lower a neutral request to a wire body and parse the wire
//! stream back into neutral events. One implementation per wire format.

use std::collections::BTreeSet;

use serde_json::Value;

use super::types::*;

pub struct WireRequest {
    /// Path appended to the provider base URL, e.g. `/chat/completions`.
    pub path: String,
    pub body: Value,
}

pub trait StreamParser: Send {
    /// Feed one decoded SSE `data:` payload.
    fn step(&mut self, data: &str) -> Result<Vec<LlmEvent>, LlmError>;
    /// Called when the stream ends; emit trailing events (tool calls, finish).
    fn halt(&mut self) -> Result<Vec<LlmEvent>, LlmError>;
}

pub trait Protocol: Send + Sync {
    fn id(&self) -> &'static str;
    fn build(&self, req: &LlmRequest) -> Result<WireRequest, LlmError>;
    fn parser(&self) -> Box<dyn StreamParser>;
}

/// Shared open/close bookkeeping so every protocol emits the same lifecycle
///.
#[derive(Default)]
pub struct Lifecycle {
    step_started: bool,
    text: BTreeSet<String>,
    reasoning: BTreeSet<String>,
}

impl Lifecycle {
    pub fn step_start(&mut self, out: &mut Vec<LlmEvent>) {
        if !self.step_started {
            self.step_started = true;
            out.push(LlmEvent::StepStart { index: 0 });
        }
    }

    pub fn text_delta(&mut self, out: &mut Vec<LlmEvent>, id: &str, text: &str) {
        self.step_start(out);
        if !self.text.contains(id) {
            self.text.insert(id.to_string());
            out.push(LlmEvent::TextStart { id: id.into() });
        }
        out.push(LlmEvent::TextDelta {
            id: id.into(),
            text: text.into(),
        });
    }

    pub fn reasoning_delta(&mut self, out: &mut Vec<LlmEvent>, id: &str, text: &str) {
        self.step_start(out);
        if !self.reasoning.contains(id) {
            self.reasoning.insert(id.to_string());
            out.push(LlmEvent::ReasoningStart { id: id.into() });
        }
        out.push(LlmEvent::ReasoningDelta {
            id: id.into(),
            text: text.into(),
        });
    }

    pub fn reasoning_end(&mut self, out: &mut Vec<LlmEvent>, id: &str, metadata: Option<Value>) {
        if self.reasoning.remove(id) {
            self.step_start(out);
            out.push(LlmEvent::ReasoningEnd {
                id: id.into(),
                metadata,
            });
        }
    }

    pub fn text_end(&mut self, out: &mut Vec<LlmEvent>, id: &str) {
        if self.text.remove(id) {
            self.step_start(out);
            out.push(LlmEvent::TextEnd { id: id.into() });
        }
    }

    pub fn finish(&mut self, out: &mut Vec<LlmEvent>, reason: FinishReason, usage: Option<Usage>) {
        self.step_start(out);
        for id in std::mem::take(&mut self.reasoning) {
            out.push(LlmEvent::ReasoningEnd { id, metadata: None });
        }
        for id in std::mem::take(&mut self.text) {
            out.push(LlmEvent::TextEnd { id });
        }
        out.push(LlmEvent::StepFinish {
            index: 0,
            reason,
            usage,
        });
        out.push(LlmEvent::Finish { reason, usage });
        self.step_started = false;
    }
}

/// Accumulates streamed tool-call argument JSON.
#[derive(Debug, Clone, Default)]
pub struct PendingTool {
    pub id: String,
    pub name: String,
    pub input: String,
}

pub fn parse_tool_input(name: &str, raw: &str) -> Result<Value, LlmError> {
    let raw = if raw.trim().is_empty() { "{}" } else { raw };
    serde_json::from_str::<Value>(raw).map_err(|e| LlmError::InvalidOutput {
        message: format!("Invalid JSON input for tool call {name}: {e}"),
    })
}

/// Emit `tool-input-end` + `tool-call` for one pending tool. Bad JSON becomes a
/// call to the `invalid` tool so the model gets feedback instead of a hard error.
pub fn finish_tool(tool: &PendingTool, out: &mut Vec<LlmEvent>) {
    out.push(LlmEvent::ToolInputEnd {
        id: tool.id.clone(),
        name: tool.name.clone(),
    });
    match parse_tool_input(&tool.name, &tool.input) {
        Ok(input) => out.push(LlmEvent::ToolCall {
            id: tool.id.clone(),
            name: tool.name.clone(),
            input,
        }),
        Err(e) => out.push(LlmEvent::ToolCall {
            id: tool.id.clone(),
            name: "invalid".into(),
            input: serde_json::json!({ "tool": tool.name, "error": e.to_string(), "raw": tool.input }),
        }),
    }
}
