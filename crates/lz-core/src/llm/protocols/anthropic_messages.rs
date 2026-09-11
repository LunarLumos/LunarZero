//! Anthropic Messages wire protocol (`/messages`, SSE) — Claude models.
//!
//! Differences from the chat-completions shape that matter here: the system
//! prompt is a top-level field, tool results travel as `tool_result` blocks
//! inside a *user* message, thinking blocks carry a signature that must be
//! replayed verbatim, and `max_tokens` is mandatory.

use serde_json::{Map, Value, json};

use crate::llm::protocol::*;
use crate::llm::types::*;

pub const PATH: &str = "/messages";
const VERSION: &str = "2023-06-01";
const IMAGE_MIMES: &[&str] = &["image/png", "image/jpeg", "image/gif", "image/webp"];
/// Fallback when neither the request nor the catalog gives an output cap.
const DEFAULT_MAX_TOKENS: u64 = 16_000;

pub struct AnthropicMessages;

fn media_block(mime: &str, data: &str) -> Result<Value, LlmError> {
    let data = data
        .strip_prefix("data:")
        .map_or(data, |rest| rest.split_once(',').map_or(rest, |(_, d)| d));
    if IMAGE_MIMES.contains(&mime) {
        return Ok(
            json!({ "type": "image", "source": { "type": "base64", "media_type": mime, "data": data } }),
        );
    }
    if mime == "application/pdf" {
        return Ok(
            json!({ "type": "document", "source": { "type": "base64", "media_type": mime, "data": data } }),
        );
    }
    Err(LlmError::InvalidRequest {
        message: format!("Claude does not accept media type {mime}"),
    })
}

fn text_block(text: &str) -> Value {
    json!({ "type": "text", "text": text })
}

/// Content blocks for a user turn (text, images, documents).
fn user_blocks(content: &[ContentPart]) -> Result<Vec<Value>, LlmError> {
    let mut blocks = Vec::new();
    for part in content {
        match part {
            ContentPart::Text { text } if !text.is_empty() => blocks.push(text_block(text)),
            ContentPart::Text { .. } => {}
            ContentPart::Media { mime, data, .. } => blocks.push(media_block(mime, data)?),
            _ => {
                return Err(LlmError::InvalidRequest {
                    message: "Claude user messages support only text and media".into(),
                });
            }
        }
    }
    Ok(blocks)
}

/// Content blocks for an assistant turn. Thinking is replayed only when it
/// carries the signature the API gave us; unsigned reasoning is dropped.
fn assistant_blocks(content: &[ContentPart]) -> Result<Vec<Value>, LlmError> {
    let mut blocks = Vec::new();
    for part in content {
        match part {
            ContentPart::Text { text } if !text.trim().is_empty() => blocks.push(text_block(text)),
            ContentPart::Text { .. } => {}
            ContentPart::Reasoning { text, metadata } => {
                if let Some(sig) = metadata.as_ref().and_then(|m| m["signature"].as_str()) {
                    blocks.push(json!({ "type": "thinking", "thinking": text, "signature": sig }));
                } else if let Some(data) = metadata.as_ref().and_then(|m| m["redacted"].as_str()) {
                    blocks.push(json!({ "type": "redacted_thinking", "data": data }));
                }
            }
            ContentPart::ToolCall { id, name, input } => {
                blocks.push(json!({ "type": "tool_use", "id": id, "name": name, "input": input }));
            }
            _ => {
                return Err(LlmError::InvalidRequest {
                    message: "Claude assistant messages support only text, reasoning and tool-call".into(),
                });
            }
        }
    }
    Ok(blocks)
}

/// `tool_result` blocks for a tool turn (they live in a user message).
fn tool_result_blocks(content: &[ContentPart]) -> Result<Vec<Value>, LlmError> {
    let mut blocks = Vec::new();
    for part in content {
        let ContentPart::ToolResult { id, result, .. } = part else {
            return Err(LlmError::InvalidRequest {
                message: "Claude tool messages support only tool-result".into(),
            });
        };
        let (body, is_error) = match result {
            ToolResultContent::Text { text } => (json!(text), false),
            ToolResultContent::Error { text } => (json!(text), true),
            ToolResultContent::Content { items } => {
                let mut inner = Vec::new();
                for item in items {
                    match item {
                        ToolContentItem::Text { text } => inner.push(text_block(text)),
                        ToolContentItem::File { mime, data, .. } => {
                            if let Ok(b) = media_block(mime, data) {
                                inner.push(b);
                            }
                        }
                    }
                }
                (Value::Array(inner), false)
            }
        };
        let mut block = json!({ "type": "tool_result", "tool_use_id": id, "content": body });
        if is_error {
            block["is_error"] = json!(true);
        }
        blocks.push(block);
    }
    Ok(blocks)
}

/// Lower the conversation to alternating user/assistant messages. Consecutive
/// same-role turns are merged (tool results + the next user text share one
/// message), which is what the API expects.
fn lower_messages(req: &LlmRequest) -> Result<Vec<Value>, LlmError> {
    let mut out: Vec<(String, Vec<Value>)> = Vec::new();
    let mut push = |role: &str, blocks: Vec<Value>| {
        if blocks.is_empty() {
            return;
        }
        match out.last_mut() {
            Some((r, existing)) if r == role => existing.extend(blocks),
            _ => out.push((role.to_string(), blocks)),
        }
    };
    for message in &req.messages {
        match message {
            LlmMessage::System { content } => push("user", user_blocks(content)?),
            LlmMessage::User { content } => push("user", user_blocks(content)?),
            LlmMessage::Assistant { content } => push("assistant", assistant_blocks(content)?),
            LlmMessage::Tool { content } => push("user", tool_result_blocks(content)?),
        }
    }
    // the conversation must start with a user turn and end with one
    if out.first().is_some_and(|(r, _)| r == "assistant") {
        out.insert(0, ("user".into(), vec![text_block("(continue)")]));
    }
    if out.last().is_some_and(|(r, _)| r == "assistant") {
        out.push(("user".into(), vec![text_block("(continue)")]));
    }
    Ok(out
        .into_iter()
        .map(|(role, content)| json!({ "role": role, "content": content }))
        .collect())
}

fn lower_tool_choice(tc: &ToolChoice) -> Option<Value> {
    match tc {
        ToolChoice::Auto => Some(json!({ "type": "auto" })),
        ToolChoice::Required => Some(json!({ "type": "any" })),
        ToolChoice::Tool { name } => Some(json!({ "type": "tool", "name": name })),
        // no tools at all is expressed by omitting them
        ToolChoice::None => None,
    }
}

/// Budget for extended thinking from a named effort; capped under max_tokens.
fn thinking_budget(effort: &str, max_tokens: u64) -> u64 {
    let want = match effort {
        "low" => 2_048,
        "medium" => 8_192,
        "high" => 16_384,
        "xhigh" | "max" => 32_768,
        _ => 8_192,
    };
    want.min(max_tokens.saturating_sub(1_024)).max(1_024)
}

impl Protocol for AnthropicMessages {
    fn id(&self) -> &'static str {
        "anthropic-messages"
    }

    fn auth_headers(&self, key: &str) -> Vec<(String, String)> {
        vec![("x-api-key".into(), key.to_string())]
    }

    fn build(&self, req: &LlmRequest) -> Result<WireRequest, LlmError> {
        let mut body = Map::new();
        body.insert("model".into(), json!(req.model_id));
        let max_tokens = req.generation.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS);
        body.insert("max_tokens".into(), json!(max_tokens));
        body.insert("stream".into(), json!(true));

        if !req.system.is_empty() {
            let mut blocks: Vec<Value> = req.system.iter().map(|s| text_block(&s.text)).collect();
            // the system prompt is the stable prefix: cache it
            if let Some(last) = blocks.last_mut() {
                last["cache_control"] = json!({ "type": "ephemeral" });
            }
            body.insert("system".into(), Value::Array(blocks));
        }
        body.insert("messages".into(), Value::Array(lower_messages(req)?));

        if !req.tools.is_empty() {
            let mut tools: Vec<Value> = req
                .tools
                .iter()
                .map(|t| json!({ "name": t.name, "description": t.description, "input_schema": t.input_schema }))
                .collect();
            if let Some(last) = tools.last_mut() {
                last["cache_control"] = json!({ "type": "ephemeral" });
            }
            body.insert("tools".into(), Value::Array(tools));
            if let Some(tc) = req.tool_choice.as_ref().and_then(lower_tool_choice) {
                body.insert("tool_choice".into(), tc);
            }
        }

        let g = &req.generation;
        if let Some(v) = g.temperature {
            body.insert("temperature".into(), json!(v));
        }
        if let Some(v) = g.top_p {
            body.insert("top_p".into(), json!(v));
        }
        if let Some(v) = g.top_k {
            body.insert("top_k".into(), json!(v));
        }
        if let Some(v) = &g.stop {
            body.insert("stop_sequences".into(), json!(v));
        }

        // provider options: anthropic.{effort | budgetTokens | thinking}
        if let Some(Value::Object(opts)) = req.provider_options.get("anthropic") {
            if let Some(t) = opts.get("thinking") {
                body.insert("thinking".into(), t.clone());
            } else if let Some(budget) = opts.get("budgetTokens").and_then(Value::as_u64) {
                body.insert(
                    "thinking".into(),
                    json!({ "type": "enabled", "budget_tokens": budget }),
                );
            } else if let Some(effort) = opts.get("effort").and_then(Value::as_str) {
                if effort != "none" {
                    let budget = thinking_budget(effort, max_tokens);
                    body.insert(
                        "thinking".into(),
                        json!({ "type": "enabled", "budget_tokens": budget }),
                    );
                }
            }
            // thinking requires the default temperature
            if body.contains_key("thinking") {
                body.remove("temperature");
                body.remove("top_p");
                body.remove("top_k");
            }
        }
        if let Some(Value::Object(raw)) = req.provider_options.get("raw") {
            for (k, v) in raw {
                body.insert(k.clone(), v.clone());
            }
        }
        Ok(WireRequest {
            path: PATH.into(),
            body: Value::Object(body),
            headers: vec![("anthropic-version".into(), VERSION.into())],
        })
    }

    fn parser(&self) -> Box<dyn StreamParser> {
        Box::new(Parser::default())
    }
}

// ───────────────────────────── stream parsing ─────────────────────────────

#[derive(Default)]
struct Parser {
    lifecycle: Lifecycle,
    /// open content blocks by index
    blocks: std::collections::BTreeMap<u64, Block>,
    /// tool calls emitted at halt so they trail any text
    tool_call_events: Vec<LlmEvent>,
    usage: Usage,
    saw_usage: bool,
    finish: Option<FinishReason>,
}

enum Block {
    Text,
    Thinking { text: String, signature: String },
    Tool(PendingTool),
}

fn map_stop(reason: &str) -> FinishReason {
    match reason {
        "end_turn" | "stop_sequence" | "pause_turn" => FinishReason::Stop,
        "max_tokens" => FinishReason::Length,
        "tool_use" => FinishReason::ToolCalls,
        "refusal" => FinishReason::ContentFilter,
        _ => FinishReason::Unknown,
    }
}

impl Parser {
    fn take_usage(&mut self, u: &Value) {
        if !u.is_object() {
            return;
        }
        self.saw_usage = true;
        if let Some(v) = u["input_tokens"].as_u64() {
            self.usage.non_cached_input_tokens = Some(v);
        }
        if let Some(v) = u["output_tokens"].as_u64() {
            self.usage.output_tokens = Some(v);
        }
        if let Some(v) = u["cache_read_input_tokens"].as_u64() {
            self.usage.cache_read_input_tokens = Some(v);
        }
        if let Some(v) = u["cache_creation_input_tokens"].as_u64() {
            self.usage.cache_write_input_tokens = Some(v);
        }
        let input = self.usage.non_cached_input_tokens.unwrap_or(0)
            + self.usage.cache_read_input_tokens.unwrap_or(0)
            + self.usage.cache_write_input_tokens.unwrap_or(0);
        self.usage.input_tokens = Some(input);
        self.usage.total_tokens = Some(input + self.usage.output_tokens.unwrap_or(0));
    }
}

impl StreamParser for Parser {
    fn step(&mut self, data: &str) -> Result<Vec<LlmEvent>, LlmError> {
        let data = data.trim();
        if data.is_empty() {
            return Ok(Vec::new());
        }
        let event: Value = serde_json::from_str(data).map_err(|e| LlmError::InvalidOutput {
            message: format!("bad SSE JSON: {e}: {data}"),
        })?;
        let mut out = Vec::new();
        match event["type"].as_str().unwrap_or("") {
            "message_start" => {
                self.take_usage(&event["message"]["usage"]);
                self.lifecycle.step_start(&mut out);
            }
            "content_block_start" => {
                let index = event["index"].as_u64().unwrap_or(0);
                let cb = &event["content_block"];
                match cb["type"].as_str().unwrap_or("") {
                    "text" => {
                        self.blocks.insert(index, Block::Text);
                        if let Some(t) = cb["text"].as_str().filter(|t| !t.is_empty()) {
                            self.lifecycle.text_delta(&mut out, &format!("text-{index}"), t);
                        }
                    }
                    "thinking" => {
                        self.blocks.insert(
                            index,
                            Block::Thinking {
                                text: String::new(),
                                signature: String::new(),
                            },
                        );
                    }
                    "redacted_thinking" => {
                        // opaque; keep it replayable but invisible
                        let id = format!("reasoning-{index}");
                        self.lifecycle.reasoning_delta(&mut out, &id, "");
                        self.lifecycle.reasoning_end(
                            &mut out,
                            &id,
                            Some(json!({ "redacted": cb["data"].as_str().unwrap_or("") })),
                        );
                    }
                    "tool_use" => {
                        let tool = PendingTool {
                            id: cb["id"].as_str().unwrap_or("").to_string(),
                            name: cb["name"].as_str().unwrap_or("").to_string(),
                            input: String::new(),
                        };
                        self.lifecycle.step_start(&mut out);
                        out.push(LlmEvent::ToolInputStart {
                            id: tool.id.clone(),
                            name: tool.name.clone(),
                        });
                        self.blocks.insert(index, Block::Tool(tool));
                    }
                    _ => {}
                }
            }
            "content_block_delta" => {
                let index = event["index"].as_u64().unwrap_or(0);
                let delta = &event["delta"];
                match (self.blocks.get_mut(&index), delta["type"].as_str().unwrap_or("")) {
                    (Some(Block::Text), "text_delta") => {
                        if let Some(t) = delta["text"].as_str().filter(|t| !t.is_empty()) {
                            self.lifecycle.text_delta(&mut out, &format!("text-{index}"), t);
                        }
                    }
                    (Some(Block::Thinking { text, .. }), "thinking_delta") => {
                        if let Some(t) = delta["thinking"].as_str() {
                            text.push_str(t);
                            self.lifecycle
                                .reasoning_delta(&mut out, &format!("reasoning-{index}"), t);
                        }
                    }
                    (Some(Block::Thinking { signature, .. }), "signature_delta") => {
                        if let Some(s) = delta["signature"].as_str() {
                            signature.push_str(s);
                        }
                    }
                    (Some(Block::Tool(tool)), "input_json_delta") => {
                        if let Some(j) = delta["partial_json"].as_str().filter(|j| !j.is_empty()) {
                            tool.input.push_str(j);
                            out.push(LlmEvent::ToolInputDelta {
                                id: tool.id.clone(),
                                name: tool.name.clone(),
                                text: j.to_string(),
                            });
                        }
                    }
                    _ => {}
                }
            }
            "content_block_stop" => {
                let index = event["index"].as_u64().unwrap_or(0);
                match self.blocks.remove(&index) {
                    Some(Block::Text) => self.lifecycle.text_end(&mut out, &format!("text-{index}")),
                    Some(Block::Thinking { signature, .. }) => {
                        let md = (!signature.is_empty()).then(|| json!({ "signature": signature }));
                        self.lifecycle
                            .reasoning_end(&mut out, &format!("reasoning-{index}"), md);
                    }
                    Some(Block::Tool(tool)) => finish_tool(&tool, &mut self.tool_call_events),
                    None => {}
                }
            }
            "message_delta" => {
                self.take_usage(&event["usage"]);
                if let Some(r) = event["delta"]["stop_reason"].as_str() {
                    self.finish = Some(map_stop(r));
                }
            }
            "error" => {
                let err = &event["error"];
                let message = err["message"].as_str().unwrap_or("stream error").to_string();
                let kind = err["type"].as_str().unwrap_or("");
                return Err(match kind {
                    "rate_limit_error" => LlmError::RateLimited {
                        message,
                        retry_after_ms: None,
                    },
                    "overloaded_error" => LlmError::Provider {
                        status: 529,
                        message,
                        retry_after_ms: None,
                        headers: Default::default(),
                        body: Some(data.to_string()),
                    },
                    _ => LlmError::Provider {
                        status: 200,
                        message,
                        retry_after_ms: None,
                        headers: Default::default(),
                        body: Some(data.to_string()),
                    },
                });
            }
            // "ping", "message_stop"
            _ => {}
        }
        Ok(out)
    }

    fn halt(&mut self) -> Result<Vec<LlmEvent>, LlmError> {
        let mut out = Vec::new();
        for (index, block) in std::mem::take(&mut self.blocks) {
            match block {
                Block::Text => self.lifecycle.text_end(&mut out, &format!("text-{index}")),
                Block::Thinking { .. } => {
                    self.lifecycle
                        .reasoning_end(&mut out, &format!("reasoning-{index}"), None)
                }
                Block::Tool(tool) => finish_tool(&tool, &mut self.tool_call_events),
            }
        }
        let has_calls = !self.tool_call_events.is_empty();
        let reason = match self.finish {
            Some(FinishReason::Stop) | None if has_calls => FinishReason::ToolCalls,
            Some(r) => r,
            None => FinishReason::Unknown,
        };
        if has_calls {
            self.lifecycle.step_start(&mut out);
        }
        out.append(&mut self.tool_call_events);
        let usage = self.saw_usage.then_some(self.usage);
        self.lifecycle.finish(&mut out, reason, usage);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req() -> LlmRequest {
        LlmRequest {
            model_id: "claude-sonnet-4-6".into(),
            system: vec![SystemBlock {
                text: "be brief".into(),
            }],
            messages: vec![
                LlmMessage::User {
                    content: vec![ContentPart::Text { text: "hi".into() }],
                },
                LlmMessage::Assistant {
                    content: vec![
                        ContentPart::Reasoning {
                            text: "think".into(),
                            metadata: Some(json!({ "signature": "sig" })),
                        },
                        ContentPart::ToolCall {
                            id: "toolu_1".into(),
                            name: "read".into(),
                            input: json!({ "filePath": "a.rs" }),
                        },
                    ],
                },
                LlmMessage::Tool {
                    content: vec![ContentPart::ToolResult {
                        id: "toolu_1".into(),
                        name: "read".into(),
                        result: ToolResultContent::Text {
                            text: "fn main(){}".into(),
                        },
                    }],
                },
                LlmMessage::User {
                    content: vec![ContentPart::Text {
                        text: "now edit".into(),
                    }],
                },
            ],
            tools: vec![ToolDef {
                name: "read".into(),
                description: "read a file".into(),
                input_schema: json!({ "type": "object" }),
            }],
            tool_choice: Some(ToolChoice::Auto),
            generation: Generation {
                max_tokens: Some(8000),
                ..Default::default()
            },
            response_format: None,
            provider_options: serde_json::from_value(json!({ "anthropic": { "effort": "high" } })).unwrap(),
            headers: Vec::new(),
        }
    }

    #[test]
    fn builds_messages_body() {
        let wire = AnthropicMessages.build(&req()).unwrap();
        assert_eq!(wire.path, "/messages");
        assert_eq!(wire.headers[0].0, "anthropic-version");
        let b = &wire.body;
        assert_eq!(b["max_tokens"], 8000);
        assert_eq!(b["system"][0]["text"], "be brief");
        assert_eq!(b["system"][0]["cache_control"]["type"], "ephemeral");
        let msgs = b["messages"].as_array().unwrap();
        // user, assistant, user (tool_result + follow-up text merged)
        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[1]["content"][0]["type"], "thinking");
        assert_eq!(msgs[1]["content"][0]["signature"], "sig");
        assert_eq!(msgs[1]["content"][1]["type"], "tool_use");
        assert_eq!(msgs[2]["content"][0]["type"], "tool_result");
        assert_eq!(msgs[2]["content"][0]["tool_use_id"], "toolu_1");
        assert_eq!(msgs[2]["content"][1]["text"], "now edit");
        assert_eq!(b["tools"][0]["input_schema"]["type"], "object");
        assert_eq!(b["thinking"]["type"], "enabled");
        assert!(b["thinking"]["budget_tokens"].as_u64().unwrap() < 8000);
        assert_eq!(AnthropicMessages.auth_headers("k")[0].0, "x-api-key");
    }

    #[test]
    fn parses_stream() {
        let mut p = AnthropicMessages.parser();
        let mut events = Vec::new();
        for line in [
            r#"{"type":"message_start","message":{"usage":{"input_tokens":10,"cache_read_input_tokens":5,"output_tokens":1}}}"#,
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"hmm"}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"abc"}}"#,
            r#"{"type":"content_block_stop","index":0}"#,
            r#"{"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}"#,
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"Hello"}}"#,
            r#"{"type":"content_block_stop","index":1}"#,
            r#"{"type":"content_block_start","index":2,"content_block":{"type":"tool_use","id":"toolu_9","name":"bash","input":{}}}"#,
            r#"{"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"{\"command\":"}}"#,
            r#"{"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"\"ls\"}"}}"#,
            r#"{"type":"content_block_stop","index":2}"#,
            r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":12}}"#,
            r#"{"type":"message_stop"}"#,
        ] {
            events.extend(p.step(line).unwrap());
        }
        events.extend(p.halt().unwrap());
        assert!(
            events
                .iter()
                .any(|e| matches!(e, LlmEvent::ReasoningDelta { text, .. } if text == "hmm"))
        );
        assert!(events.iter().any(
            |e| matches!(e, LlmEvent::ReasoningEnd { metadata: Some(m), .. } if m["signature"] == "abc")
        ));
        assert!(
            events
                .iter()
                .any(|e| matches!(e, LlmEvent::TextDelta { text, .. } if text == "Hello"))
        );
        let call = events.iter().find_map(|e| match e {
            LlmEvent::ToolCall { name, input, .. } => Some((name.clone(), input.clone())),
            _ => None,
        });
        assert_eq!(call, Some(("bash".to_string(), json!({ "command": "ls" }))));
        let finish = events.iter().find_map(|e| match e {
            LlmEvent::Finish { reason, usage } => Some((*reason, *usage)),
            _ => None,
        });
        let (reason, usage) = finish.unwrap();
        assert_eq!(reason, FinishReason::ToolCalls);
        let u = usage.unwrap();
        assert_eq!(u.input_tokens, Some(15));
        assert_eq!(u.cache_read_input_tokens, Some(5));
        assert_eq!(u.output_tokens, Some(12));
    }

    #[test]
    fn stream_error_maps() {
        let mut p = AnthropicMessages.parser();
        let err = p
            .step(r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#)
            .unwrap_err();
        assert!(err.retryable());
    }
}
