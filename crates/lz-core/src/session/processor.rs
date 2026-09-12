//! One provider turn: stream `LlmEvent`s into persisted parts, execute tool
//! calls concurrently, retry transient failures, and report the outcome to
//! the runner..

use std::collections::HashMap;
use std::sync::Arc;

use lz_schema::Event;
use lz_schema::ids::{self, Prefix};
use lz_schema::session::*;
use serde_json::{Map, Value};
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use super::retry;
use crate::agent::Agent;
use crate::engine::Engine;
use crate::llm::types::*;
use crate::provider::Model;
use crate::storage::now_ms;
use crate::tool::registry::ResolvedTool;
use crate::tool::{ToolCtx, ToolError, ToolProgress, ToolResult};

const DOOM_LOOP_THRESHOLD: usize = 3;

pub struct ProcessInput {
    pub engine: Arc<Engine>,
    pub session_id: String,
    pub assistant: AssistantMessage,
    pub model: Model,
    pub agent: Arc<Agent>,
    pub system: Vec<SystemBlock>,
    pub messages: Vec<LlmMessage>,
    pub tools: Vec<ResolvedTool>,
    pub tool_choice: Option<ToolChoice>,
    pub response_format: Option<ResponseFormat>,
    pub variant: Option<String>,
    pub cancel: CancellationToken,
    pub history: Arc<Vec<MessageWithParts>>,
    pub bypass_agent_check: bool,
    /// Free-pool routing: switch to another pool model on rate limits/outages.
    pub route: Option<RouteInput>,
}

#[derive(Debug, Clone)]
pub struct RouteInput {
    pub strategy: crate::provider::pool::Strategy,
    pub need: crate::provider::router::Need,
    pub sticky_minutes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepOutcome {
    Stop,
    Compact,
    Continue,
}

pub struct StepResult {
    pub outcome: StepOutcome,
    pub message: AssistantMessage,
    /// Captured `StructuredOutput` tool call, if any.
    pub structured: Option<Value>,
}

struct ToolCall {
    part: Part,
    done: bool,
}

struct Ctx {
    engine: Arc<Engine>,
    session_id: String,
    message: AssistantMessage,
    model: Model,
    agent: Arc<Agent>,
    tools: HashMap<String, Arc<dyn crate::tool::Tool>>,
    calls: HashMap<String, ToolCall>,
    current_text: Option<Part>,
    reasoning: HashMap<String, Part>,
    snapshot: Option<String>,
    blocked: bool,
    should_break_on_deny: bool,
    needs_compaction: bool,
    structured: Option<Value>,
    join: JoinSet<(String, Result<ToolResult, ToolError>)>,
    progress_tx: mpsc::UnboundedSender<(String, ToolProgress)>,
    tool_cancel: CancellationToken,
    history: Arc<Vec<MessageWithParts>>,
    bypass_agent_check: bool,
}

fn usage_to_tokens(model: &Model, usage: Option<Usage>) -> (Tokens, f64) {
    let u = usage.unwrap_or_default();
    let input = u.input_tokens.unwrap_or(0) as f64;
    let output = u.output_tokens.unwrap_or(0) as f64;
    let reasoning = u.reasoning_tokens.unwrap_or(0) as f64;
    let cache_read = u.cache_read_input_tokens.unwrap_or(0) as f64;
    let cache_write = u.cache_write_input_tokens.unwrap_or(0) as f64;
    let adjusted_input = (input - cache_read - cache_write).max(0.0);
    let tokens = Tokens {
        total: u.total_tokens.map(|t| t as f64),
        input: adjusted_input,
        output: (output - reasoning).max(0.0),
        reasoning,
        cache: CacheTokens {
            read: cache_read,
            write: cache_write,
        },
    };
    let c = &model.cost;
    let cost = (tokens.input * c.input
        + tokens.output * c.output
        + tokens.cache.read * c.cache_read
        + tokens.cache.write * c.cache_write
        + tokens.reasoning * c.output)
        / 1_000_000.0;
    (tokens, cost.max(0.0))
}

impl Ctx {
    fn sessions(&self) -> &super::SessionService {
        &self.engine.sessions
    }

    fn new_part(&self, kind: PartKind) -> Part {
        Part {
            id: ids::ascending(Prefix::Part),
            session_id: self.session_id.clone(),
            message_id: self.message.id.clone(),
            kind,
        }
    }

    async fn save_part(&self, part: &Part) {
        let _ = self.sessions().update_part(part.clone()).await;
    }

    async fn ensure_tool_call(&mut self, id: &str, name: &str) {
        if self.calls.contains_key(id) {
            return;
        }
        let part = self.new_part(PartKind::Tool {
            call_id: id.into(),
            tool: name.into(),
            state: ToolState::Pending {
                input: Value::Object(Map::new()),
                raw: String::new(),
            },
            metadata: None,
        });
        self.save_part(&part).await;
        self.calls.insert(id.into(), ToolCall { part, done: false });
    }

    async fn finish_reasoning(&mut self, id: &str) {
        if let Some(mut part) = self.reasoning.remove(id) {
            if let PartKind::Reasoning { time, .. } = &mut part.kind {
                time.end = Some(now_ms());
            }
            self.save_part(&part).await;
        }
    }

    async fn is_doom_loop(&self, name: &str, input: &Value) -> bool {
        // recent tool parts across this session (history + this message)
        let mut recent: Vec<(&str, &Value)> = Vec::new();
        for m in self.history.iter() {
            for p in &m.parts {
                if let PartKind::Tool { tool, state, .. } = &p.kind
                    && !matches!(state, ToolState::Pending { .. })
                {
                    recent.push((tool.as_str(), state.input()));
                }
            }
        }
        let mut current: Vec<&Part> = self.calls.values().map(|c| &c.part).collect();
        current.sort_by(|a, b| a.id.cmp(&b.id));
        for p in current {
            if let PartKind::Tool { tool, state, .. } = &p.kind
                && !matches!(state, ToolState::Pending { .. })
            {
                recent.push((tool.as_str(), state.input()));
            }
        }
        if recent.len() < DOOM_LOOP_THRESHOLD {
            return false;
        }
        let input_s = input.to_string();
        recent
            .iter()
            .rev()
            .take(DOOM_LOOP_THRESHOLD)
            .all(|(t, i)| *t == name && i.to_string() == input_s)
    }

    async fn on_tool_call(
        &mut self,
        id: String,
        name: String,
        input: Value,
        extra: Option<Value>,
    ) -> Result<(), String> {
        if self.message.summary == Some(true) {
            return Err(format!("Tool call not allowed while generating summary: {name}"));
        }
        self.ensure_tool_call(&id, &name).await;
        let input = if input.is_object() {
            input
        } else {
            serde_json::json!({ "value": input })
        };
        // mark running
        {
            let call = self.calls.get_mut(&id).expect("ensured");
            if let PartKind::Tool {
                tool,
                state,
                metadata,
                ..
            } = &mut call.part.kind
            {
                *tool = name.clone();
                *state = ToolState::Running {
                    input: input.clone(),
                    title: None,
                    metadata: None,
                    time: ToolTimeRunning { start: now_ms() },
                };
                if let Some(ex) = extra {
                    *metadata = Some(serde_json::json!({ "provider": ex }));
                }
            }
            let part = call.part.clone();
            self.save_part(&part).await;
        }

        if name != "invalid" && name != "StructuredOutput" && self.is_doom_loop(&name, &input).await {
            let ask = self
                .engine
                .permissions
                .ask(crate::permission::AskInput {
                    session_id: self.session_id.clone(),
                    permission: "doom_loop".into(),
                    patterns: vec![name.clone()],
                    always: vec![name.clone()],
                    metadata: serde_json::json!({ "tool": name, "input": input })
                        .as_object()
                        .cloned()
                        .unwrap_or_default(),
                    tool: Some(ToolRef {
                        message_id: self.message.id.clone(),
                        call_id: id.clone(),
                    }),
                    ruleset: self.agent.permission.clone(),
                    force: false,
                })
                .await;
            if let Err(e) = ask {
                self.fail_tool(&id, ToolError::Permission(e)).await;
                return Ok(());
            }
        }

        if name == "StructuredOutput" {
            self.structured = Some(input.clone());
            self.complete_tool(
                &id,
                ToolResult::text("StructuredOutput", "Structured output captured"),
            )
            .await;
            return Ok(());
        }

        let Some(tool) = self.tools.get(&name).cloned() else {
            let known: Vec<&str> = self.tools.keys().map(String::as_str).collect();
            self.fail_tool(
                &id,
                ToolError::Invalid(format!(
                    "Unknown tool '{name}'. Available tools: {}",
                    known.join(", ")
                )),
            )
            .await;
            return Ok(());
        };
        let (progress_tx, mut progress_rx) = mpsc::unbounded_channel::<ToolProgress>();
        let forward = self.progress_tx.clone();
        let call_id = id.clone();
        tokio::spawn(async move {
            while let Some(p) = progress_rx.recv().await {
                let _ = forward.send((call_id.clone(), p));
            }
        });
        let ctx = ToolCtx {
            session_id: self.session_id.clone(),
            message_id: self.message.id.clone(),
            call_id: id.clone(),
            agent: self.agent.clone(),
            cancel: self.tool_cancel.child_token(),
            engine: self.engine.clone(),
            progress: progress_tx,
            messages: self.history.clone(),
            bypass_agent_check: self.bypass_agent_check,
        };
        let id2 = id.clone();
        self.join.spawn(async move {
            let result = tool.execute(ctx, input).await;
            (id2, result)
        });
        Ok(())
    }

    async fn apply_progress(&mut self, call_id: &str, p: ToolProgress) {
        let Some(call) = self.calls.get_mut(call_id) else {
            return;
        };
        if call.done {
            return;
        }
        if let PartKind::Tool {
            state: ToolState::Running { title, metadata, .. },
            ..
        } = &mut call.part.kind
        {
            if p.title.is_some() {
                *title = p.title;
            }
            if p.metadata.is_some() {
                *metadata = p.metadata;
            }
        }
        let part = call.part.clone();
        self.save_part(&part).await;
    }

    async fn complete_tool(&mut self, call_id: &str, result: ToolResult) {
        let Some(call) = self.calls.get_mut(call_id) else {
            return;
        };
        if let PartKind::Tool { state, .. } = &mut call.part.kind {
            let start = match state {
                ToolState::Running { time, .. } => time.start,
                _ => now_ms(),
            };
            let input = state.input().clone();
            *state = ToolState::Completed {
                input,
                output: result.output,
                title: result.title,
                metadata: result.metadata,
                time: ToolTimeCompleted {
                    start,
                    end: now_ms(),
                    compacted: None,
                },
                attachments: if result.attachments.is_empty() {
                    None
                } else {
                    Some(result.attachments)
                },
            };
        }
        call.done = true;
        let part = call.part.clone();
        self.save_part(&part).await;
    }

    async fn fail_tool(&mut self, call_id: &str, error: ToolError) {
        if error.is_rejection() {
            self.blocked = self.should_break_on_deny;
        }
        let Some(call) = self.calls.get_mut(call_id) else {
            return;
        };
        if let PartKind::Tool { state, .. } = &mut call.part.kind {
            let (start, metadata) = match state {
                ToolState::Running { time, metadata, .. } => (time.start, metadata.clone()),
                _ => (now_ms(), None),
            };
            let input = state.input().clone();
            *state = ToolState::Error {
                input,
                error: error.to_string(),
                metadata,
                time: ToolTimeError { start, end: now_ms() },
            };
        }
        call.done = true;
        let part = call.part.clone();
        self.save_part(&part).await;
    }

    /// Wait for every spawned tool to finish, applying results as they land.
    async fn settle_tools(&mut self) {
        while let Some(joined) = self.join.join_next().await {
            match joined {
                Ok((id, Ok(result))) => self.complete_tool(&id, result).await,
                Ok((id, Err(e))) => self.fail_tool(&id, e).await,
                Err(e) => tracing::error!("tool task failed: {e}"),
            }
        }
    }

    async fn handle(&mut self, event: LlmEvent) -> Result<(), String> {
        match event {
            LlmEvent::StepStart { .. } => {
                if self.snapshot.is_none() {
                    self.snapshot = self.engine.snapshot_track().await;
                }
                let part = self.new_part(PartKind::StepStart {
                    snapshot: self.snapshot.clone(),
                });
                self.save_part(&part).await;
            }
            LlmEvent::TextStart { .. } => {
                let part = self.new_part(PartKind::Text {
                    text: String::new(),
                    synthetic: false,
                    ignored: false,
                    time: Some(PartTime {
                        start: now_ms(),
                        end: None,
                    }),
                    metadata: None,
                });
                self.save_part(&part).await;
                self.current_text = Some(part);
            }
            LlmEvent::TextDelta { text, .. } => {
                let sessions = self.engine.sessions.clone();
                if let Some(part) = &mut self.current_text {
                    let mut looping = false;
                    if let PartKind::Text { text: t, .. } = &mut part.kind {
                        t.push_str(&text);
                        // check every ~400 chars; cheap and early enough
                        let len = t.len();
                        if (len / 400) != ((len - text.len()) / 400)
                            && (t.len() > super::loop_guard::MAX_TEXT_CHARS && !self.tools.is_empty()
                                || super::loop_guard::is_looping(t))
                        {
                            looping = true;
                        }
                    }
                    let _ = sessions.update_part_delta(part, "text", &text).await;
                    if looping {
                        return Err(LOOP_MARKER.into());
                    }
                }
            }
            LlmEvent::TextEnd { .. } => {
                if let Some(mut part) = self.current_text.take() {
                    if let PartKind::Text { time, .. } = &mut part.kind {
                        let end = now_ms();
                        *time = Some(PartTime {
                            start: time.as_ref().map(|t| t.start).unwrap_or(end),
                            end: Some(end),
                        });
                    }
                    self.save_part(&part).await;
                }
            }
            LlmEvent::ReasoningStart { id } => {
                if self.reasoning.contains_key(&id) {
                    return Ok(());
                }
                let part = self.new_part(PartKind::Reasoning {
                    text: String::new(),
                    metadata: None,
                    time: PartTime {
                        start: now_ms(),
                        end: None,
                    },
                });
                self.save_part(&part).await;
                self.reasoning.insert(id, part);
            }
            LlmEvent::ReasoningDelta { id, text } => {
                let sessions = self.engine.sessions.clone();
                if let Some(part) = self.reasoning.get_mut(&id) {
                    if let PartKind::Reasoning { text: t, .. } = &mut part.kind {
                        t.push_str(&text);
                    }
                    let _ = sessions.update_part_delta(part, "text", &text).await;
                }
            }
            LlmEvent::ReasoningEnd { id, metadata } => {
                if let Some(part) = self.reasoning.get_mut(&id)
                    && let (PartKind::Reasoning { metadata: m, .. }, Some(md)) = (&mut part.kind, metadata)
                {
                    *m = Some(md);
                }
                self.finish_reasoning(&id).await;
            }
            LlmEvent::ToolInputStart { id, name } | LlmEvent::ToolInputDelta { id, name, .. } => {
                if self.message.summary == Some(true) {
                    return Err(format!("Tool call not allowed while generating summary: {name}"));
                }
                self.ensure_tool_call(&id, &name).await;
            }
            LlmEvent::ToolInputEnd { .. } => {}
            LlmEvent::ToolCall {
                id,
                name,
                input,
                extra,
            } => {
                self.on_tool_call(id, name, input, extra).await?;
            }
            LlmEvent::StepFinish { reason, usage, .. } => {
                // all tools must settle before the step is closed
                self.settle_tools().await;
                let ids: Vec<String> = self.reasoning.keys().cloned().collect();
                for id in ids {
                    self.finish_reasoning(&id).await;
                }
                let completed_snapshot = self.engine.snapshot_track().await;
                let (tokens, cost) = usage_to_tokens(&self.model, usage);
                if self.model.pool.is_some() {
                    self.engine.router.record_tokens(
                        &self.model,
                        tokens.effective_total() as u64 + tokens.output as u64,
                    );
                }
                self.message.finish = Some(reason.as_str().to_string());
                self.message.cost += cost;
                self.message.tokens = tokens;
                let part = self.new_part(PartKind::StepFinish {
                    reason: reason.as_str().to_string(),
                    snapshot: completed_snapshot,
                    cost,
                    tokens,
                });
                self.save_part(&part).await;
                let _ = self
                    .sessions()
                    .update_message(Message::Assistant(self.message.clone()))
                    .await;
                if let Some(snap) = self.snapshot.take()
                    && let Some((hash, files)) = self.engine.snapshot_patch(&snap).await
                    && !files.is_empty()
                {
                    let part = self.new_part(PartKind::Patch { hash, files });
                    self.save_part(&part).await;
                }
                if self.message.summary != Some(true)
                    && super::runner::is_overflow(&self.engine.config(), &tokens, &self.model)
                {
                    self.needs_compaction = true;
                }
            }
            LlmEvent::Finish { .. } => {}
        }
        Ok(())
    }

    /// Close everything that is still open (called on every exit path).
    async fn cleanup(&mut self) {
        if let Some(snap) = self.snapshot.take()
            && let Some((hash, files)) = self.engine.snapshot_patch(&snap).await
            && !files.is_empty()
        {
            let part = self.new_part(PartKind::Patch { hash, files });
            self.save_part(&part).await;
        }
        if let Some(mut part) = self.current_text.take() {
            if let PartKind::Text { time, .. } = &mut part.kind {
                let end = now_ms();
                *time = Some(PartTime {
                    start: time.as_ref().map(|t| t.start).unwrap_or(end),
                    end: Some(end),
                });
            }
            self.save_part(&part).await;
        }
        let ids: Vec<String> = self.reasoning.keys().cloned().collect();
        for id in ids {
            self.finish_reasoning(&id).await;
        }
        // give in-flight tools a moment, then mark the rest interrupted
        self.tool_cancel.cancel();
        let _ = tokio::time::timeout(std::time::Duration::from_millis(250), self.settle_tools()).await;
        self.join.abort_all();
        let pending: Vec<String> = self
            .calls
            .iter()
            .filter(|(_, c)| !c.done)
            .map(|(k, _)| k.clone())
            .collect();
        for id in pending {
            if let Some(call) = self.calls.get_mut(&id) {
                if let PartKind::Tool { state, .. } = &mut call.part.kind {
                    let (start, mut metadata) = match state {
                        ToolState::Running { time, metadata, .. } => {
                            (time.start, metadata.clone().unwrap_or(Value::Object(Map::new())))
                        }
                        _ => (now_ms(), Value::Object(Map::new())),
                    };
                    if let Value::Object(m) = &mut metadata {
                        m.insert("interrupted".into(), Value::Bool(true));
                    }
                    let input = state.input().clone();
                    *state = ToolState::Error {
                        input,
                        error: "Tool execution aborted".into(),
                        metadata: Some(metadata),
                        time: ToolTimeError { start, end: now_ms() },
                    };
                }
                call.done = true;
                let part = call.part.clone();
                self.save_part(&part).await;
            }
        }
        self.message.time.completed = Some(now_ms());
        let _ = self
            .sessions()
            .update_message(Message::Assistant(self.message.clone()))
            .await;
    }

    /// Drop the text part being streamed (used when the model looped).
    async fn discard_text(&mut self) {
        if let Some(part) = self.current_text.take() {
            let _ = self.sessions().remove_part(&part).await;
        }
    }

    /// Failover: re-point the assistant message at another model.
    async fn switch_model(&mut self, model: Model, reason: &str) {
        self.model = model;
        self.message.provider_id = self.model.provider_id.clone();
        self.message.model_id = self.model.id.clone();
        let _ = self
            .sessions()
            .update_message(Message::Assistant(self.message.clone()))
            .await;
        self.engine.bus.publish(Event::ModelRouted {
            session_id: self.session_id.clone(),
            message_id: self.message.id.clone(),
            provider_id: self.model.provider_id.clone(),
            model_id: self.model.id.clone(),
            reason: reason.to_string(),
        });
    }

    async fn halt(&mut self, error: MessageError) {
        let is_overflow = matches!(error, MessageError::ContextOverflow { .. });
        if is_overflow && self.message.summary != Some(true) && self.engine.config().compaction_auto() {
            self.needs_compaction = true;
            self.engine.bus.publish(Event::SessionError {
                session_id: Some(self.session_id.clone()),
                error,
            });
            return;
        }
        self.message.error = Some(error.clone());
        if is_overflow {
            self.message.finish = Some("error".into());
        }
        self.engine.bus.publish(Event::SessionError {
            session_id: Some(self.session_id.clone()),
            error,
        });
    }
}

/// `ctx.handle` returns this when the model is repeating itself.
pub const LOOP_MARKER: &str = "model stuck repeating itself";

/// Errors that justify switching pool models: limits, outages, dead model ids,
/// bad keys — anything except aborts, context overflow and content policy.
fn failover_worthy(e: &LlmError) -> bool {
    match e {
        LlmError::Aborted | LlmError::ContextOverflow { .. } | LlmError::ContentPolicy { .. } => false,
        LlmError::InvalidOutput { message } => message == LOOP_MARKER,
        LlmError::Provider { .. } => true,
        _ => true,
    }
}

fn short_error(e: &LlmError) -> String {
    // providers wrap the useful text in `{"error":{"message":...}}`
    let json_message = |text: &str| -> Option<String> {
        let v: Value = serde_json::from_str(text.trim()).ok()?;
        let v = if let Value::Array(a) = &v {
            a.first().cloned()?
        } else {
            v
        };
        v.pointer("/error/message")
            .or_else(|| v.get("message"))
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    let s = match e {
        LlmError::RateLimited { .. } => "rate limited".to_string(),
        LlmError::InvalidOutput { message } if message == LOOP_MARKER => "kept repeating itself".to_string(),
        LlmError::Provider {
            status,
            message,
            body,
            ..
        } => {
            let detail = json_message(message)
                .or_else(|| body.as_deref().and_then(json_message))
                .unwrap_or_else(|| message.split_whitespace().collect::<Vec<_>>().join(" "));
            format!("HTTP {status}: {detail}")
        }
        LlmError::Authentication { .. } => "authentication failed".into(),
        LlmError::Timeout { .. } => "timed out".into(),
        LlmError::Network { .. } => "network error".into(),
        other => other.to_string(),
    };
    s.chars().take(80).collect()
}

fn llm_error_to_message_error(e: &LlmError, provider_id: &str) -> MessageError {
    match e {
        LlmError::Aborted => MessageError::Aborted {
            message: "Aborted".into(),
        },
        LlmError::Authentication { message } => MessageError::ProviderAuth {
            provider_id: provider_id.into(),
            message: message.clone(),
        },
        LlmError::ContextOverflow { message } => MessageError::ContextOverflow {
            message: message.clone(),
            response_body: None,
        },
        LlmError::ContentPolicy { message } => MessageError::ContentFilter {
            message: message.clone(),
        },
        LlmError::RateLimited { message, .. } => MessageError::Api(ApiErrorData {
            message: message.clone(),
            status_code: Some(429),
            is_retryable: true,
            response_headers: None,
            response_body: None,
            metadata: None,
        }),
        LlmError::Provider {
            status,
            message,
            headers,
            body,
            ..
        } => MessageError::Api(ApiErrorData {
            message: message.clone(),
            status_code: Some(*status as u32),
            is_retryable: e.retryable(),
            response_headers: if headers.is_empty() {
                None
            } else {
                Some(headers.clone())
            },
            response_body: body.clone(),
            metadata: None,
        }),
        LlmError::Network { message } | LlmError::Timeout { message } => MessageError::Api(ApiErrorData {
            message: message.clone(),
            status_code: None,
            is_retryable: true,
            response_headers: None,
            response_body: None,
            metadata: None,
        }),
        LlmError::InvalidRequest { message } | LlmError::InvalidOutput { message } => MessageError::Unknown {
            message: message.clone(),
            r#ref: None,
        },
    }
}

pub async fn process(input: ProcessInput) -> StepResult {
    let engine = input.engine.clone();
    let session_id = input.session_id.clone();
    let should_break_on_deny = engine
        .config()
        .experimental
        .as_ref()
        .and_then(|e| e.continue_loop_on_deny)
        != Some(true);
    let (progress_tx, mut progress_rx) = mpsc::unbounded_channel();
    let tools: HashMap<String, Arc<dyn crate::tool::Tool>> = input
        .tools
        .iter()
        .map(|t| (t.def.name.clone(), t.tool.clone()))
        .collect();
    let mut ctx = Ctx {
        engine: engine.clone(),
        session_id: session_id.clone(),
        message: input.assistant,
        model: input.model.clone(),
        agent: input.agent.clone(),
        tools,
        calls: HashMap::new(),
        current_text: None,
        reasoning: HashMap::new(),
        snapshot: engine.snapshot_track().await,
        blocked: false,
        should_break_on_deny,
        needs_compaction: false,
        structured: None,
        join: JoinSet::new(),
        progress_tx,
        tool_cancel: input.cancel.child_token(),
        history: input.history,
        bypass_agent_check: input.bypass_agent_check,
    };

    engine.status.set(&engine.bus, &session_id, SessionStatus::Busy);

    let tool_defs: Vec<_> = input.tools.iter().map(|t| t.def.clone()).collect();
    let build_request = |model: &Model| -> LlmRequest {
        let mut request = LlmRequest {
            model_id: model.api_id.clone(),
            system: input.system.clone(),
            messages: input.messages.clone(),
            tools: tool_defs.clone(),
            tool_choice: input.tool_choice.clone(),
            generation: Generation {
                max_tokens: Some(crate::provider::transform::max_output_tokens(model)),
                temperature: input
                    .agent
                    .temperature
                    .or_else(|| crate::provider::transform::temperature(model)),
                top_p: input
                    .agent
                    .top_p
                    .or_else(|| crate::provider::transform::top_p(model)),
                ..Default::default()
            },
            response_format: input.response_format.clone(),
            provider_options: crate::provider::transform::provider_options(model, input.variant.as_deref()),
            headers: Vec::new(),
        };
        if !model.temperature {
            request.generation.temperature = None;
        }
        request
    };
    let mut request = build_request(&ctx.model);
    // pool models already tried in this step (failover never returns to them)
    let mut tried: Vec<String> = vec![format!("{}/{}", ctx.model.provider_id, ctx.model.id)];
    let mut waits: u32 = 0;
    let failover_enabled = input.route.is_some();

    let mut attempt: u32 = 0;
    loop {
        attempt += 1;
        let (protocol, endpoint) = match engine.registry().endpoint(&ctx.model) {
            Ok(v) => v,
            Err(e) => {
                ctx.halt(MessageError::ProviderAuth {
                    provider_id: ctx.model.provider_id.clone(),
                    message: e,
                })
                .await;
                break;
            }
        };
        if ctx.model.pool.is_some() {
            engine.router.record_request(&ctx.model, 0);
        }
        let mut rx =
            engine
                .registry()
                .client()
                .stream(protocol, endpoint, request.clone(), input.cancel.clone());
        let mut failure: Option<LlmError> = None;
        let started = std::time::Instant::now();
        let mut first_token: Option<std::time::Instant> = None;
        let mut think = crate::llm::think_tags::ThinkTagFilter::default();
        loop {
            tokio::select! {
                biased;
                _ = input.cancel.cancelled() => { failure = Some(LlmError::Aborted); break; }
                Some((call_id, p)) = progress_rx.recv() => { ctx.apply_progress(&call_id, p).await; }
                ev = rx.recv() => match ev {
                    None => {
                        for event in think.finish() {
                            let _ = ctx.handle(event).await;
                        }
                        break;
                    }
                    Some(Ok(event)) => {
                        if first_token.is_none()
                            && matches!(event, LlmEvent::TextDelta { .. } | LlmEvent::ReasoningDelta { .. } | LlmEvent::ToolInputStart { .. } | LlmEvent::ToolCall { .. })
                        {
                            first_token = Some(std::time::Instant::now());
                        }
                        // inline <thought>…</thought> → reasoning, flushed before step end
                        let events = match &event {
                            LlmEvent::StepFinish { .. } | LlmEvent::Finish { .. } => {
                                let mut v = think.finish();
                                v.push(event);
                                v
                            }
                            _ => think.push(event),
                        };
                        let mut failed = false;
                        for event in events {
                            if let Err(msg) = ctx.handle(event).await {
                                failure = Some(LlmError::InvalidOutput { message: msg });
                                failed = true;
                                break;
                            }
                        }
                        if failed || ctx.needs_compaction { break; }
                    }
                    Some(Err(e)) => { failure = Some(e); break; }
                },
            }
        }
        // drain any straggling progress messages
        while let Ok((call_id, p)) = progress_rx.try_recv() {
            ctx.apply_progress(&call_id, p).await;
        }
        match failure {
            None => {
                if ctx.model.pool.is_some()
                    && let Some(first) = first_token
                {
                    let ttft = (first.duration_since(started).as_millis() as u64).max(1);
                    let gen_ms = first.elapsed().as_millis() as u64;
                    let out_tokens = (ctx.message.tokens.output + ctx.message.tokens.reasoning) as u64;
                    engine.router.record_latency(&ctx.model, ttft, out_tokens, gen_ms);
                }
                break;
            }
            Some(LlmError::Aborted) => {
                ctx.halt(MessageError::Aborted {
                    message: "Aborted".into(),
                })
                .await;
                break;
            }
            Some(err) => {
                let looped = matches!(&err, LlmError::InvalidOutput { message } if message == LOOP_MARKER);
                if looped {
                    // throw the repeated text away so it never reaches the history
                    ctx.discard_text().await;
                    engine.bus.publish(Event::SessionError {
                        session_id: Some(session_id.clone()),
                        error: MessageError::Unknown {
                            message: format!(
                                "{} — {}",
                                LOOP_MARKER,
                                if failover_enabled {
                                    "switching model"
                                } else {
                                    "stopped"
                                }
                            ),
                            r#ref: None,
                        },
                    });
                }
                let produced_output = !looped
                    && (ctx.current_text.is_some() || !ctx.calls.is_empty() || !ctx.reasoning.is_empty());
                // free-pool failover: cool the model down and jump to the next
                // candidate right away (only while nothing was streamed yet)
                if failover_enabled && !produced_output && failover_worthy(&err) {
                    let cooldown = engine.router.record_failure(&ctx.model, &err);
                    let route = input.route.as_ref().unwrap();
                    let registry = engine.registry();
                    let next = engine.router.pick_with(
                        &registry,
                        route.strategy,
                        &route.need,
                        &session_id,
                        &tried,
                        0,
                        &crate::provider::router::Policy::from_config(
                            engine.config().pool.as_ref().and_then(|p| p.policy.as_ref()),
                        ),
                    );
                    if let Some(pick) = next {
                        tracing::warn!(
                            from = %tried.last().cloned().unwrap_or_default(),
                            to = %format!("{}/{}", pick.model.provider_id, pick.model.id),
                            cooldown_secs = cooldown.as_secs(),
                            "pool failover: {err}"
                        );
                        let reason = short_error(&err);
                        ctx.switch_model(pick.model, &reason).await;
                        tried.push(format!("{}/{}", ctx.model.provider_id, ctx.model.id));
                        request = build_request(&ctx.model);
                        if route.sticky_minutes > 0 {
                            engine.router.clear_sticky(&session_id);
                        }
                        continue;
                    }
                    // nothing free right now: wait for the soonest model (≤ 2 min)
                    // rather than failing, and say what we are waiting for
                    const MAX_WAIT_MS: u64 = 120_000;
                    // a model we already tried is fine to wait for (a 429 with
                    // retry-after is exactly that case), at most a few times per step
                    if waits < 3
                        && let Some((model, wait, why)) =
                            engine.router.soonest(&registry, &route.need, &[], MAX_WAIT_MS)
                    {
                        waits += 1;
                        let wait = wait.max(1_000) + 500;
                        let key = format!("{}/{}", model.provider_id, model.id);
                        let message = format!("waiting {}s for {key} ({why})", wait / 1000);
                        tracing::warn!("{message}");
                        engine.status.set(
                            &engine.bus,
                            &session_id,
                            SessionStatus::Retry {
                                attempt,
                                message: message.clone(),
                                next: now_ms() + wait,
                            },
                        );
                        tokio::select! {
                            _ = tokio::time::sleep(std::time::Duration::from_millis(wait)) => {}
                            _ = input.cancel.cancelled() => {
                                ctx.halt(MessageError::Aborted { message: "Aborted".into() }).await;
                                break;
                            }
                        }
                        engine.status.set(&engine.bus, &session_id, SessionStatus::Busy);
                        tried.retain(|t| *t != key);
                        // the waited-for model may still lose to a better one that freed up
                        if let Some(pick) = engine.router.pick_with(
                            &registry,
                            route.strategy,
                            &route.need,
                            &session_id,
                            &tried,
                            0,
                            &crate::provider::router::Policy::from_config(
                                engine.config().pool.as_ref().and_then(|p| p.policy.as_ref()),
                            ),
                        ) {
                            ctx.switch_model(pick.model, &format!("waited for {key}")).await;
                            tried.push(format!("{}/{}", ctx.model.provider_id, ctx.model.id));
                            request = build_request(&ctx.model);
                            continue;
                        }
                    }
                    // truly exhausted: explain every model instead of showing a raw provider error
                    let blocked = engine.router.explain(&registry, &route.need, &[]);
                    if !blocked.is_empty() || !tried.is_empty() {
                        let mut lines: Vec<String> = Vec::new();
                        if !blocked.iter().any(|(m, _, _)| tried.last() == Some(m)) {
                            lines.push(format!(
                                "{}: {}",
                                tried.last().cloned().unwrap_or_default(),
                                short_error(&err)
                            ));
                        }
                        for (model, why, wait) in blocked.iter().take(8) {
                            lines.push(match wait {
                                Some(ms) => format!("{model}: {why} (free in {}s)", ms / 1000),
                                None => format!("{model}: {why}"),
                            });
                        }
                        let msg = format!(
                            "No free-pool model can take this request right now (~{}k tokens). {}",
                            route.need.tokens / 1000,
                            lines.join(" · ")
                        );
                        ctx.halt(MessageError::Unknown {
                            message: msg,
                            r#ref: None,
                        })
                        .await;
                        break;
                    }
                }
                let retry_msg = if produced_output {
                    None
                } else {
                    retry::retryable(&err)
                };
                if let (Some(message), true) = (retry_msg, attempt <= retry::RETRY_MAX_RETRIES) {
                    let delay = retry::delay_ms(attempt, Some(&err), rand::random::<f64>());
                    let next = now_ms() + delay;
                    let api_err = llm_error_to_message_error(&err, &ctx.model.provider_id);
                    let part = ctx.new_part(PartKind::Retry {
                        attempt,
                        error: api_err,
                        time: RetryTime { created: now_ms() },
                    });
                    ctx.save_part(&part).await;
                    engine.status.set(
                        &engine.bus,
                        &session_id,
                        SessionStatus::Retry {
                            attempt,
                            message: message.clone(),
                            next,
                        },
                    );
                    tracing::warn!(attempt, delay, "retrying provider call: {message}");
                    tokio::select! {
                        _ = tokio::time::sleep(std::time::Duration::from_millis(delay)) => {}
                        _ = input.cancel.cancelled() => {
                            ctx.halt(MessageError::Aborted { message: "Aborted".into() }).await;
                            break;
                        }
                    }
                    engine.status.set(&engine.bus, &session_id, SessionStatus::Busy);
                    continue;
                }
                ctx.halt(llm_error_to_message_error(&err, &ctx.model.provider_id))
                    .await;
                break;
            }
        }
    }

    ctx.cleanup().await;
    let outcome = if ctx.needs_compaction {
        StepOutcome::Compact
    } else if ctx.blocked || ctx.message.error.is_some() {
        StepOutcome::Stop
    } else {
        StepOutcome::Continue
    };
    StepResult {
        outcome,
        message: ctx.message,
        structured: ctx.structured,
    }
}
