//! OpenAI Chat Completions wire protocol (`/chat/completions`, SSE).
//!. Used for OpenAI,
//! OpenRouter, Groq, DeepSeek, Ollama, LM Studio, and any compatible server.

use std::collections::BTreeMap;

use serde_json::{Map, Value, json};

use crate::llm::protocol::*;
use crate::llm::types::*;

pub const PATH: &str = "/chat/completions";
const IMAGE_MIMES: &[&str] = &["image/png", "image/jpeg", "image/gif", "image/webp"];

pub struct OpenAiChat;

fn join_text(parts: &[ContentPart]) -> String {
    parts
        .iter()
        .filter_map(|p| match p {
            ContentPart::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
}

fn media_to_image(mime: &str, data: &str) -> Result<Value, LlmError> {
    if !IMAGE_MIMES.contains(&mime) {
        return Err(LlmError::InvalidRequest {
            message: format!("OpenAI Chat does not support media type {mime}"),
        });
    }
    let url = if data.starts_with("data:") {
        data.to_string()
    } else {
        format!("data:{mime};base64,{data}")
    };
    Ok(json!({ "type": "image_url", "image_url": { "url": url } }))
}

fn lower_user(content: &[ContentPart]) -> Result<Value, LlmError> {
    let mut items = Vec::new();
    let mut all_text = true;
    for part in content {
        match part {
            ContentPart::Text { text } => items.push(json!({ "type": "text", "text": text })),
            ContentPart::Media { mime, data, .. } => {
                all_text = false;
                items.push(media_to_image(mime, data)?);
            }
            _ => {
                return Err(LlmError::InvalidRequest {
                    message: "OpenAI Chat user messages support only text and media".into(),
                });
            }
        }
    }
    if all_text {
        return Ok(json!({ "role": "user", "content": join_text(content) }));
    }
    Ok(json!({ "role": "user", "content": items }))
}

fn lower_assistant(content: &[ContentPart], send_reasoning: bool) -> Result<Value, LlmError> {
    let mut text = String::new();
    let mut reasoning = String::new();
    let mut tool_calls = Vec::new();
    for part in content {
        match part {
            ContentPart::Text { text: t } => text.push_str(t),
            ContentPart::Reasoning { text: t, .. } => reasoning.push_str(t),
            ContentPart::ToolCall {
                id,
                name,
                input,
                extra,
            } => {
                let mut call = json!({
                    "id": id,
                    "type": "function",
                    "function": { "name": name, "arguments": input.to_string() }
                });
                // Gemini 3 rejects a later turn unless the thought signature it
                // attached to the call comes back on it
                if let Some(Value::Object(ex)) = extra {
                    for (k, v) in ex {
                        call[k] = v.clone();
                    }
                }
                tool_calls.push(call);
            }
            _ => {
                return Err(LlmError::InvalidRequest {
                    message: "OpenAI Chat assistant messages support only text, reasoning and tool-call"
                        .into(),
                });
            }
        }
    }
    let mut m = Map::new();
    m.insert("role".into(), json!("assistant"));
    m.insert(
        "content".into(),
        if text.is_empty() {
            Value::Null
        } else {
            Value::String(text)
        },
    );
    if !tool_calls.is_empty() {
        m.insert("tool_calls".into(), Value::Array(tool_calls));
    }
    // Replaying reasoning costs tokens on every later request and several
    // providers reject it; only send it when the model opts in.
    if send_reasoning && !reasoning.is_empty() {
        m.insert("reasoning_content".into(), Value::String(reasoning));
    }
    Ok(Value::Object(m))
}

/// Tool results become `role: tool` messages; any file attachments are
/// returned separately so they can be flushed as a trailing user message.
fn lower_tool(content: &[ContentPart]) -> Result<(Vec<Value>, Vec<Value>), LlmError> {
    let mut messages = Vec::new();
    let mut images = Vec::new();
    for part in content {
        let ContentPart::ToolResult { id, result, .. } = part else {
            return Err(LlmError::InvalidRequest {
                message: "OpenAI Chat tool messages support only tool-result".into(),
            });
        };
        let text = match result {
            ToolResultContent::Text { text } | ToolResultContent::Error { text } => text.clone(),
            ToolResultContent::Content { items } => {
                let mut texts = Vec::new();
                for item in items {
                    match item {
                        ToolContentItem::Text { text } => texts.push(text.clone()),
                        ToolContentItem::File { mime, data, .. } => {
                            if let Ok(img) = media_to_image(mime, data) {
                                images.push(img);
                            }
                        }
                    }
                }
                texts.join("\n")
            }
        };
        messages.push(json!({ "role": "tool", "tool_call_id": id, "content": text }));
    }
    Ok((messages, images))
}

fn lower_messages(req: &LlmRequest) -> Result<Vec<Value>, LlmError> {
    let send_reasoning = req
        .provider_options
        .get("openai")
        .and_then(|o| o.get("sendReasoning"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let mut out = Vec::new();
    if !req.system.is_empty() {
        let text = req
            .system
            .iter()
            .map(|s| s.text.as_str())
            .collect::<Vec<_>>()
            .join("\n\n");
        out.push(json!({ "role": "system", "content": text }));
    }
    let mut pending_images: Vec<Value> = Vec::new();
    for message in &req.messages {
        match message {
            LlmMessage::System { content } => {
                // mid-conversation system update → appended to the previous/next user turn
                let text = join_text(content);
                if !pending_images.is_empty() {
                    let mut items = std::mem::take(&mut pending_images);
                    items.push(json!({ "type": "text", "text": text }));
                    out.push(json!({ "role": "user", "content": items }));
                    continue;
                }
                match out.last_mut() {
                    Some(prev) if prev["role"] == "user" && prev["content"].is_string() => {
                        let joined = format!("{}\n{}", prev["content"].as_str().unwrap_or(""), text);
                        prev["content"] = Value::String(joined);
                    }
                    Some(prev) if prev["role"] == "user" && prev["content"].is_array() => {
                        if let Some(arr) = prev["content"].as_array_mut() {
                            arr.push(json!({ "type": "text", "text": text }));
                        }
                    }
                    _ => out.push(json!({ "role": "user", "content": text })),
                }
            }
            LlmMessage::Tool { content } => {
                let (msgs, images) = lower_tool(content)?;
                out.extend(msgs);
                pending_images.extend(images);
            }
            LlmMessage::User { content } => {
                if !pending_images.is_empty() {
                    out.push(json!({ "role": "user", "content": std::mem::take(&mut pending_images) }));
                }
                out.push(lower_user(content)?);
            }
            LlmMessage::Assistant { content } => {
                if !pending_images.is_empty() {
                    out.push(json!({ "role": "user", "content": std::mem::take(&mut pending_images) }));
                }
                out.push(lower_assistant(content, send_reasoning)?);
            }
        }
    }
    if !pending_images.is_empty() {
        out.push(json!({ "role": "user", "content": pending_images }));
    }
    Ok(out)
}

fn lower_tool_choice(tc: &ToolChoice) -> Value {
    match tc {
        ToolChoice::Auto => json!("auto"),
        ToolChoice::None => json!("none"),
        ToolChoice::Required => json!("required"),
        ToolChoice::Tool { name } => json!({ "type": "function", "function": { "name": name } }),
    }
}

impl Protocol for OpenAiChat {
    fn id(&self) -> &'static str {
        "openai-chat"
    }

    fn build(&self, req: &LlmRequest) -> Result<WireRequest, LlmError> {
        let mut body = Map::new();
        body.insert("model".into(), json!(req.model_id));
        body.insert("messages".into(), Value::Array(lower_messages(req)?));
        if !req.tools.is_empty() {
            let tools: Vec<Value> = req
                .tools
                .iter()
                .map(|t| {
                    json!({
                        "type": "function",
                        "function": { "name": t.name, "description": t.description, "parameters": t.input_schema }
                    })
                })
                .collect();
            body.insert("tools".into(), Value::Array(tools));
        }
        if let Some(tc) = &req.tool_choice {
            body.insert("tool_choice".into(), lower_tool_choice(tc));
        }
        body.insert("stream".into(), json!(true));
        body.insert("stream_options".into(), json!({ "include_usage": true }));
        let g = &req.generation;
        if let Some(v) = g.max_tokens {
            body.insert("max_tokens".into(), json!(v));
        }
        if let Some(v) = g.temperature {
            body.insert("temperature".into(), json!(v));
        }
        if let Some(v) = g.top_p {
            body.insert("top_p".into(), json!(v));
        }
        if let Some(v) = g.frequency_penalty {
            body.insert("frequency_penalty".into(), json!(v));
        }
        if let Some(v) = g.presence_penalty {
            body.insert("presence_penalty".into(), json!(v));
        }
        if let Some(v) = g.seed {
            body.insert("seed".into(), json!(v));
        }
        if let Some(v) = &g.stop {
            body.insert("stop".into(), json!(v));
        }
        if let Some(ResponseFormat::JsonSchema { name, schema, strict }) = &req.response_format {
            body.insert(
                "response_format".into(),
                json!({ "type": "json_schema", "json_schema": { "name": name, "schema": schema, "strict": strict } }),
            );
        }
        // provider options: openai.{reasoningEffort, store, ...} and any raw passthrough
        if let Some(Value::Object(openai)) = req.provider_options.get("openai") {
            if let Some(effort) = openai.get("reasoningEffort").and_then(Value::as_str) {
                body.insert("reasoning_effort".into(), json!(effort));
            }
            if let Some(store) = openai.get("store") {
                body.insert("store".into(), store.clone());
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
            headers: Vec::new(),
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
    tools: BTreeMap<u64, PendingTool>,
    /// Emitted at halt (after all deltas) so the model's tool calls trail text.
    tool_call_events: Vec<LlmEvent>,
    usage: Option<Usage>,
    finish: Option<FinishReason>,
}

fn map_finish(reason: &str) -> FinishReason {
    match reason {
        "stop" => FinishReason::Stop,
        "length" => FinishReason::Length,
        "content_filter" => FinishReason::ContentFilter,
        "function_call" | "tool_calls" => FinishReason::ToolCalls,
        _ => FinishReason::Unknown,
    }
}

fn map_usage(u: &Value) -> Option<Usage> {
    if !u.is_object() {
        return None;
    }
    let prompt = u["prompt_tokens"].as_u64();
    let completion = u["completion_tokens"].as_u64();
    let cached = u["prompt_tokens_details"]["cached_tokens"].as_u64();
    let reasoning = u["completion_tokens_details"]["reasoning_tokens"].as_u64();
    let total = u["total_tokens"].as_u64().or_else(|| match (prompt, completion) {
        (None, None) => None,
        (p, c) => Some(p.unwrap_or(0) + c.unwrap_or(0)),
    });
    Some(Usage {
        input_tokens: prompt,
        output_tokens: completion,
        total_tokens: total,
        non_cached_input_tokens: prompt.map(|p| p.saturating_sub(cached.unwrap_or(0))),
        cache_read_input_tokens: cached,
        cache_write_input_tokens: None,
        reasoning_tokens: reasoning,
    })
}

impl StreamParser for Parser {
    fn step(&mut self, data: &str) -> Result<Vec<LlmEvent>, LlmError> {
        let data = data.trim();
        if data.is_empty() || data == "[DONE]" {
            return Ok(Vec::new());
        }
        let event: Value = serde_json::from_str(data).map_err(|e| LlmError::InvalidOutput {
            message: format!("bad SSE JSON: {e}: {data}"),
        })?;
        if let Some(err) = event.get("error") {
            let message = err["message"].as_str().unwrap_or(&err.to_string()).to_string();
            return Err(LlmError::Provider {
                status: 200,
                message,
                retry_after_ms: None,
                headers: BTreeMap::new(),
                body: Some(data.to_string()),
            });
        }
        let mut out = Vec::new();
        if let Some(u) = map_usage(&event["usage"]) {
            self.usage = Some(u);
        }
        let choice = event["choices"].get(0).cloned().unwrap_or(Value::Null);
        let finish_now = choice["finish_reason"].as_str().map(map_finish);
        let delta = &choice["delta"];

        // reasoning: `reasoning_content` (DeepSeek/OpenAI-compatible) or `reasoning` (OpenRouter et al.)
        let reasoning = delta["reasoning_content"]
            .as_str()
            .or_else(|| delta["reasoning"].as_str());
        if let Some(r) = reasoning.filter(|r| !r.is_empty()) {
            self.lifecycle.reasoning_delta(&mut out, "reasoning-0", r);
        }
        if let Some(c) = delta["content"].as_str().filter(|c| !c.is_empty()) {
            self.lifecycle.reasoning_end(&mut out, "reasoning-0", None);
            self.lifecycle.text_delta(&mut out, "text-0", c);
        }
        if let Some(tool_deltas) = delta["tool_calls"].as_array() {
            if !tool_deltas.is_empty() {
                self.lifecycle.reasoning_end(&mut out, "reasoning-0", None);
            }
            for td in tool_deltas {
                let index = td["index"].as_u64().unwrap_or(0);
                let id = td["id"].as_str().map(str::to_string);
                let name = td["function"]["name"].as_str().map(str::to_string);
                let args = td["function"]["arguments"].as_str().unwrap_or("");
                let existing = self.tools.get(&index);
                let id = id.or_else(|| existing.map(|t| t.id.clone()));
                let name = name.or_else(|| existing.map(|t| t.name.clone()));
                let (Some(id), Some(name)) = (id, name) else {
                    // some servers stream a nameless chunk first; keep index but skip
                    continue;
                };
                let is_new = existing.is_none();
                let tool = self.tools.entry(index).or_insert_with(|| PendingTool {
                    id: id.clone(),
                    name: name.clone(),
                    input: String::new(),
                    extra: None,
                });
                if let Some(ec) = td.get("extra_content").filter(|v| v.is_object()) {
                    tool.extra = Some(json!({ "extra_content": ec }));
                }
                if tool.id.is_empty() {
                    tool.id = id;
                }
                if tool.name.is_empty() {
                    tool.name = name;
                }
                if is_new {
                    self.lifecycle.step_start(&mut out);
                    out.push(LlmEvent::ToolInputStart {
                        id: tool.id.clone(),
                        name: tool.name.clone(),
                    });
                }
                if !args.is_empty() {
                    tool.input.push_str(args);
                    out.push(LlmEvent::ToolInputDelta {
                        id: tool.id.clone(),
                        name: tool.name.clone(),
                        text: args.to_string(),
                    });
                }
            }
        }

        if let Some(reason) = finish_now {
            if self.finish.is_none() && !self.tools.is_empty() {
                let mut events = Vec::new();
                for tool in std::mem::take(&mut self.tools).values() {
                    finish_tool(tool, &mut events);
                }
                self.tool_call_events = events;
            }
            self.finish = Some(reason);
        }
        Ok(out)
    }

    fn halt(&mut self) -> Result<Vec<LlmEvent>, LlmError> {
        let mut out = Vec::new();
        // tools that never saw a finish_reason (some servers omit it)
        if !self.tools.is_empty() {
            for tool in std::mem::take(&mut self.tools).values() {
                finish_tool(tool, &mut self.tool_call_events);
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
        self.lifecycle.finish(&mut out, reason, self.usage);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(chunks: &[&str]) -> Vec<LlmEvent> {
        let mut p = OpenAiChat.parser();
        let mut out = Vec::new();
        for c in chunks {
            out.extend(p.step(c).unwrap());
        }
        out.extend(p.halt().unwrap());
        out
    }

    #[test]
    fn text_stream() {
        let ev = run(&[
            r#"{"choices":[{"delta":{"role":"assistant","content":""}}]}"#,
            r#"{"choices":[{"delta":{"content":"Hel"}}]}"#,
            r#"{"choices":[{"delta":{"content":"lo"}}]}"#,
            r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#,
            r#"{"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":2,"prompt_tokens_details":{"cached_tokens":4}}}"#,
            "[DONE]",
        ]);
        assert!(matches!(ev[0], LlmEvent::StepStart { .. }));
        assert!(matches!(&ev[1], LlmEvent::TextStart { id } if id == "text-0"));
        assert!(matches!(&ev[2], LlmEvent::TextDelta { text, .. } if text == "Hel"));
        assert!(matches!(&ev[3], LlmEvent::TextDelta { text, .. } if text == "lo"));
        assert!(matches!(ev[4], LlmEvent::TextEnd { .. }));
        match &ev[5] {
            LlmEvent::StepFinish { reason, usage, .. } => {
                assert_eq!(*reason, FinishReason::Stop);
                let u = usage.unwrap();
                assert_eq!(u.input_tokens, Some(10));
                assert_eq!(u.cache_read_input_tokens, Some(4));
                assert_eq!(u.non_cached_input_tokens, Some(6));
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(ev[6], LlmEvent::Finish { .. }));
    }

    #[test]
    fn tool_call_stream_with_reasoning() {
        let ev = run(&[
            r#"{"choices":[{"delta":{"reasoning_content":"think"}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"read","arguments":"{\"fi"}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"lePath\":\"a\"}"}}]}}]}"#,
            r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
        ]);
        let kinds: Vec<&str> = ev
            .iter()
            .map(|e| match e {
                LlmEvent::StepStart { .. } => "step-start",
                LlmEvent::ReasoningStart { .. } => "reasoning-start",
                LlmEvent::ReasoningDelta { .. } => "reasoning-delta",
                LlmEvent::ReasoningEnd { .. } => "reasoning-end",
                LlmEvent::ToolInputStart { .. } => "tool-input-start",
                LlmEvent::ToolInputDelta { .. } => "tool-input-delta",
                LlmEvent::ToolInputEnd { .. } => "tool-input-end",
                LlmEvent::ToolCall { .. } => "tool-call",
                LlmEvent::StepFinish { .. } => "step-finish",
                LlmEvent::Finish { .. } => "finish",
                _ => "other",
            })
            .collect();
        assert_eq!(
            kinds,
            vec![
                "step-start",
                "reasoning-start",
                "reasoning-delta",
                "reasoning-end",
                "tool-input-start",
                "tool-input-delta",
                "tool-input-delta",
                "tool-input-end",
                "tool-call",
                "step-finish",
                "finish"
            ]
        );
        match &ev[8] {
            LlmEvent::ToolCall { name, input, .. } => {
                assert_eq!(name, "read");
                assert_eq!(input["filePath"], "a");
            }
            _ => panic!(),
        }
        assert!(matches!(
            ev[9],
            LlmEvent::StepFinish {
                reason: FinishReason::ToolCalls,
                ..
            }
        ));
    }

    #[test]
    fn bad_tool_json_routes_to_invalid_tool() {
        let ev = run(&[
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c","function":{"name":"bash","arguments":"{oops"}}]}}]}"#,
            r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#,
        ]);
        let call = ev
            .iter()
            .find(|e| matches!(e, LlmEvent::ToolCall { .. }))
            .unwrap();
        match call {
            LlmEvent::ToolCall { name, input, .. } => {
                assert_eq!(name, "invalid");
                assert_eq!(input["tool"], "bash");
            }
            _ => unreachable!(),
        }
        // stop + tool calls → tool-calls
        assert!(ev.iter().any(|e| matches!(
            e,
            LlmEvent::Finish {
                reason: FinishReason::ToolCalls,
                ..
            }
        )));
    }

    #[test]
    fn builds_body() {
        let req = LlmRequest {
            model_id: "gpt-4o".into(),
            system: vec![SystemBlock { text: "sys".into() }],
            messages: vec![
                LlmMessage::User {
                    content: vec![ContentPart::Text { text: "hi".into() }],
                },
                LlmMessage::Assistant {
                    content: vec![ContentPart::ToolCall {
                        id: "c1".into(),
                        name: "read".into(),
                        input: json!({"filePath": "x"}),
                        extra: None,
                    }],
                },
                LlmMessage::Tool {
                    content: vec![ContentPart::ToolResult {
                        id: "c1".into(),
                        name: "read".into(),
                        result: ToolResultContent::Text {
                            text: "contents".into(),
                        },
                    }],
                },
            ],
            tools: vec![ToolDef {
                name: "read".into(),
                description: "d".into(),
                input_schema: json!({"type":"object"}),
            }],
            ..Default::default()
        };
        let wire = OpenAiChat.build(&req).unwrap();
        assert_eq!(wire.path, "/chat/completions");
        let msgs = wire.body["messages"].as_array().unwrap();
        assert_eq!(msgs[0]["role"], "system");
        assert_eq!(msgs[1]["content"], "hi");
        assert_eq!(
            msgs[2]["tool_calls"][0]["function"]["arguments"],
            "{\"filePath\":\"x\"}"
        );
        assert_eq!(msgs[3]["role"], "tool");
        assert_eq!(wire.body["stream"], true);
    }

    #[test]
    fn tool_call_extra_content_round_trips() {
        // Gemini attaches a thought signature to the call and requires it back
        let mut p = OpenAiChat.parser();
        let mut events = Vec::new();
        events.extend(p.step(r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","type":"function","function":{"name":"read","arguments":"{\"filePath\":\"a\"}"},"extra_content":{"google":{"thought_signature":"SIG"}}}]}}]}"#).unwrap());
        events.extend(
            p.step(r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#)
                .unwrap(),
        );
        events.extend(p.halt().unwrap());
        let extra = events
            .iter()
            .find_map(|e| match e {
                LlmEvent::ToolCall { extra, .. } => Some(extra.clone()),
                _ => None,
            })
            .flatten()
            .expect("extra captured");
        assert_eq!(extra["extra_content"]["google"]["thought_signature"], "SIG");
        let req = LlmRequest {
            model_id: "gemini".into(),
            messages: vec![LlmMessage::Assistant {
                content: vec![ContentPart::ToolCall {
                    id: "c1".into(),
                    name: "read".into(),
                    input: serde_json::json!({ "filePath": "a" }),
                    extra: Some(extra),
                }],
            }],
            ..Default::default()
        };
        let wire = OpenAiChat.build(&req).unwrap();
        assert_eq!(
            wire.body["messages"][0]["tool_calls"][0]["extra_content"]["google"]["thought_signature"],
            "SIG"
        );
    }
}
