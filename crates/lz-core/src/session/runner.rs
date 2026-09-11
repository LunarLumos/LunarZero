//! Prompt intake and the outer multi-step loop.

use std::sync::Arc;

use dashmap::DashMap;
use lz_schema::Event;
use lz_schema::ids::{self, Prefix};
use lz_schema::session::*;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use super::history::{self, ToModelOptions};
use super::processor::{self, ProcessInput, StepOutcome};
use super::system;
use crate::agent::Agent;
use crate::engine::Engine;
use crate::llm::types::*;
use crate::provider::Model;
use crate::storage::now_ms;

#[derive(Debug, thiserror::Error)]
pub enum PromptError {
    #[error("session is busy")]
    Busy,
    #[error("agent not found: {0}")]
    AgentNotFound(String),
    #[error("no model available: {0}")]
    NoModel(String),
    #[error("{0}")]
    Storage(#[from] crate::storage::StorageError),
}

struct RunHandle {
    cancel: CancellationToken,
    done: watch::Receiver<bool>,
}

#[derive(Default)]
pub struct SessionRunner {
    running: DashMap<String, RunHandle>,
}

/// Context is full when the last step's total tokens
/// reach the usable input window minus a reserve for the reply.
pub fn is_overflow(config: &lz_schema::config::Config, tokens: &Tokens, model: &Model) -> bool {
    if !config.compaction_auto() {
        return false;
    }
    let max_output = crate::provider::transform::max_output_tokens(model) as f64;
    let reserved = config
        .compaction
        .as_ref()
        .and_then(|c| c.reserved)
        .map(|r| r as f64)
        .unwrap_or_else(|| 20_000f64.min(max_output));
    let usable = match model.limit.input {
        Some(input) if input > 0.0 => input - reserved,
        _ => model.limit.context - max_output.max(reserved),
    };
    if usable <= 0.0 {
        return false;
    }
    tokens.effective_total() >= usable
}

impl SessionRunner {
    pub fn is_running(&self, session_id: &str) -> bool {
        self.running.get(session_id).is_some_and(|h| !*h.done.borrow())
    }

    pub async fn abort(&self, session_id: &str) {
        if let Some(h) = self.running.get(session_id) {
            h.cancel.cancel();
            let mut done = h.done.clone();
            drop(h);
            let _ = tokio::time::timeout(std::time::Duration::from_secs(5), async {
                while !*done.borrow() {
                    if done.changed().await.is_err() {
                        break;
                    }
                }
            })
            .await;
        }
    }

    /// Wait until the session's run loop exits.
    pub async fn wait(&self, session_id: &str) {
        let Some(h) = self.running.get(session_id) else {
            return;
        };
        let mut done = h.done.clone();
        drop(h);
        while !*done.borrow() {
            if done.changed().await.is_err() {
                break;
            }
        }
    }

    /// Spawn the loop for a session if it isn't already running.
    pub fn ensure_running(&self, engine: Arc<Engine>, session_id: String) {
        if self.is_running(&session_id) {
            return;
        }
        let cancel = CancellationToken::new();
        let (done_tx, done_rx) = watch::channel(false);
        self.running.insert(
            session_id.clone(),
            RunHandle {
                cancel: cancel.clone(),
                done: done_rx,
            },
        );
        let sid = session_id.clone();
        tokio::spawn(async move {
            let result = run_loop(engine.clone(), sid.clone(), cancel).await;
            if let Err(e) = result {
                tracing::error!(session = sid, "run loop failed: {e}");
                engine.bus.publish(Event::SessionError {
                    session_id: Some(sid.clone()),
                    error: MessageError::Unknown {
                        message: e.to_string(),
                        r#ref: None,
                    },
                });
            }
            engine.status.set(&engine.bus, &sid, SessionStatus::Idle);
            engine.permissions.cancel_session(&sid);
            engine.questions.cancel_session(&sid);
            engine.runner.running.remove(&sid);
            let _ = done_tx.send(true);
        });
    }
}

/// Resolve the model for a new prompt: explicit → agent default → session's
/// last model → global default.
async fn resolve_model(
    engine: &Engine,
    session: &SessionInfo,
    agent: &Agent,
    requested: Option<&ModelRef>,
) -> Result<Model, PromptError> {
    let registry = engine.registry();
    if let Some(r) = requested {
        return registry
            .get(&r.provider_id, &r.model_id)
            .cloned()
            .ok_or_else(|| PromptError::NoModel(format!("{}/{}", r.provider_id, r.model_id)));
    }
    if let Some(r) = &agent.model
        && let Some(m) = registry.get(&r.provider_id, &r.model_id)
    {
        return Ok(m.clone());
    }
    if let Some(m) = &session.model
        && let Some(found) = registry.get(&m.provider_id, &m.id)
    {
        return Ok(found.clone());
    }
    registry.default_model(&engine.config()).cloned().ok_or_else(|| {
        PromptError::NoModel(
            "no provider is configured. Run `lz auth login` or set an API key env var".into(),
        )
    })
}

/// Persist a user message + parts and kick the loop.
pub async fn prompt(
    engine: Arc<Engine>,
    session_id: &str,
    req: PromptRequest,
) -> Result<UserMessage, PromptError> {
    if engine.runner.is_running(session_id) {
        return Err(PromptError::Busy);
    }
    let mut session = engine.sessions.get(session_id).await?;
    if session.revert.is_some() {
        if let Err(e) = super::revert::cleanup(&engine, &session).await {
            tracing::warn!("revert cleanup failed: {e}");
        }
        session = engine.sessions.get(session_id).await?;
    }
    let agents = engine.agents();
    let agent = match &req.agent {
        Some(name) => agents
            .get(name)
            .ok_or_else(|| PromptError::AgentNotFound(name.clone()))?
            .clone(),
        None => match &session.agent {
            Some(name) if agents.get(name).is_some() => agents.get(name).unwrap().clone(),
            _ => agents
                .default_agent(engine.config().default_agent.as_deref())
                .clone(),
        },
    };
    let model = resolve_model(&engine, &session, &agent, req.model.as_ref()).await?;
    let variant = req
        .variant
        .clone()
        .or_else(|| agent.variant.clone().filter(|v| model.variants.contains_key(v)));

    let info = UserMessage {
        id: req
            .message_id
            .clone()
            .unwrap_or_else(|| ids::ascending(Prefix::Message)),
        session_id: session_id.into(),
        time: UserTime { created: now_ms() },
        format: req.format.clone(),
        summary: None,
        agent: agent.name.clone(),
        model: ModelRef {
            provider_id: model.provider_id.clone(),
            model_id: model.id.clone(),
            variant: variant.clone(),
        },
        system: req.system.clone(),
        tools: req.tools.clone(),
    };

    // remember agent/model on the session
    let changed = session.agent.as_deref() != Some(&agent.name)
        || session
            .model
            .as_ref()
            .is_none_or(|m| m.provider_id != model.provider_id || m.id != model.id);
    if changed {
        let a = agent.name.clone();
        let m = SessionModel {
            id: model.id.clone(),
            provider_id: model.provider_id.clone(),
            variant: variant.clone(),
        };
        engine
            .sessions
            .modify(session_id, move |s| {
                s.agent = Some(a);
                s.model = Some(m);
            })
            .await?;
    }
    if let Some(tools) = &req.tools {
        // per-prompt tool toggles become a session-level ruleset
        let mut rules = Vec::new();
        for (tool, enabled) in tools {
            let action = if *enabled {
                lz_schema::permission::Action::Allow
            } else {
                lz_schema::permission::Action::Deny
            };
            let key = if matches!(tool.as_str(), "write" | "edit" | "patch") {
                "edit"
            } else {
                tool.as_str()
            };
            rules.push(lz_schema::permission::Rule::new(key, "*", action));
        }
        engine
            .sessions
            .modify(session_id, move |s| s.permission = Some(rules))
            .await?;
    }

    engine
        .sessions
        .update_message(Message::User(info.clone()))
        .await?;

    for input in req.parts {
        for part in resolve_part(&engine, &info, input).await {
            engine.sessions.update_part(part).await?;
        }
    }

    if !req.no_reply {
        engine
            .runner
            .ensure_running(engine.clone(), session_id.to_string());
    }
    Ok(info)
}

fn mk_text(info: &UserMessage, text: String, synthetic: bool) -> Part {
    Part {
        id: ids::ascending(Prefix::Part),
        session_id: info.session_id.clone(),
        message_id: info.id.clone(),
        kind: PartKind::Text {
            text,
            synthetic,
            ignored: false,
            time: None,
            metadata: None,
        },
    }
}

/// Turn a client part into stored parts. Text files are inlined as synthetic
/// text (mirroring a `read` call) so the model sees the content directly.
async fn resolve_part(engine: &Engine, info: &UserMessage, input: PartInput) -> Vec<Part> {
    match input {
        PartInput::Text {
            id,
            text,
            synthetic,
            ignored,
        } => vec![Part {
            id: id.unwrap_or_else(|| ids::ascending(Prefix::Part)),
            session_id: info.session_id.clone(),
            message_id: info.id.clone(),
            kind: PartKind::Text {
                text,
                synthetic,
                ignored,
                time: None,
                metadata: None,
            },
        }],
        PartInput::Agent { id, name, source } => vec![Part {
            id: id.unwrap_or_else(|| ids::ascending(Prefix::Part)),
            session_id: info.session_id.clone(),
            message_id: info.id.clone(),
            kind: PartKind::Agent { name, source },
        }],
        PartInput::Subtask {
            id,
            prompt,
            description,
            agent,
            model,
            command,
        } => vec![Part {
            id: id.unwrap_or_else(|| ids::ascending(Prefix::Part)),
            session_id: info.session_id.clone(),
            message_id: info.id.clone(),
            kind: PartKind::Subtask {
                prompt,
                description,
                agent,
                model,
                command,
            },
        }],
        PartInput::File {
            id,
            mime,
            filename,
            url,
            source,
        } => {
            let part_id = id.unwrap_or_else(|| ids::ascending(Prefix::Part));
            let file_part = |mime: String, url: String| Part {
                id: part_id.clone(),
                session_id: info.session_id.clone(),
                message_id: info.id.clone(),
                kind: PartKind::File {
                    mime,
                    filename: filename.clone(),
                    url,
                    source: source.clone(),
                },
            };
            if let Some(path) = url.strip_prefix("file://") {
                let path = path.split('?').next().unwrap_or(path);
                let abs = engine.resolve_path(path);
                if abs.is_dir() {
                    let listing = crate::tool::builtins::read::list_dir(&abs).unwrap_or_default();
                    return vec![
                        mk_text(
                            info,
                            format!(
                                "Called the Read tool with the following input: {{\"filePath\":\"{}\"}}",
                                abs.display()
                            ),
                            true,
                        ),
                        mk_text(info, listing, true),
                        file_part("application/x-directory".into(), url.clone()),
                    ];
                }
                match crate::tool::builtins::read::read_for_prompt(&abs) {
                    Ok(crate::tool::builtins::read::PromptRead::Text(text)) => {
                        return vec![
                            mk_text(
                                info,
                                format!(
                                    "Called the Read tool with the following input: {{\"filePath\":\"{}\"}}",
                                    abs.display()
                                ),
                                true,
                            ),
                            mk_text(info, text, true),
                            file_part("text/plain".into(), url.clone()),
                        ];
                    }
                    Ok(crate::tool::builtins::read::PromptRead::Binary { mime, data_url }) => {
                        return vec![file_part(mime, data_url)];
                    }
                    Err(e) => {
                        engine.bus.publish(Event::SessionError {
                            session_id: Some(info.session_id.clone()),
                            error: MessageError::Unknown {
                                message: e.clone(),
                                r#ref: None,
                            },
                        });
                        return vec![mk_text(
                            info,
                            format!("[Failed to read file {}: {e}]", abs.display()),
                            true,
                        )];
                    }
                }
            }
            if url.starts_with("data:")
                && mime == "text/plain"
                && let Some((_, data)) = url.split_once(',')
            {
                let text = base64_decode(data).unwrap_or_default();
                return vec![
                    mk_text(
                        info,
                        format!(
                            "Called the Read tool with the following input: {{\"filePath\":{}}}",
                            serde_json::to_string(&filename.clone().unwrap_or_default()).unwrap_or_default()
                        ),
                        true,
                    ),
                    mk_text(info, text, true),
                    file_part(mime, url),
                ];
            }
            vec![file_part(mime, url)]
        }
    }
}

fn base64_decode(s: &str) -> Option<String> {
    let table = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let mut buf = 0u32;
    let mut bits = 0;
    for c in s.bytes() {
        if c == b'=' {
            break;
        }
        let v = table.iter().position(|&t| t == c)? as u32;
        buf = (buf << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((buf >> bits) & 0xff) as u8);
        }
    }
    String::from_utf8(out).ok()
}

/// Last assistant message with parts.
pub async fn last_assistant(engine: &Engine, session_id: &str) -> Option<MessageWithParts> {
    let msgs = engine.sessions.messages(session_id, None, None).await.ok()?;
    msgs.into_iter()
        .rev()
        .find(|m| matches!(m.info, Message::Assistant(_)))
}

async fn run_loop(engine: Arc<Engine>, session_id: String, cancel: CancellationToken) -> anyhow::Result<()> {
    let mut step: u32 = 0;
    let mut structured: Option<serde_json::Value> = None;
    loop {
        if cancel.is_cancelled() {
            break;
        }
        engine.status.set(&engine.bus, &session_id, SessionStatus::Busy);
        let all = engine.sessions.messages(&session_id, None, None).await?;
        let mut msgs = history::filter_compacted(all);
        let latest = history::latest(&msgs);
        let Some(last_user) = latest.user.cloned() else {
            anyhow::bail!("no user message in session");
        };
        let last_assistant = latest.assistant.cloned();
        let last_finished = latest.finished.cloned();
        let tasks: Vec<Part> = latest.tasks.into_iter().cloned().collect();

        // termination check
        if let Some(a) = &last_assistant {
            let has_tool_calls = msgs
                .iter()
                .find(|m| m.info.id() == a.id)
                .map(|m| {
                    m.parts.iter().any(|p| match &p.kind {
                        PartKind::Tool { state, .. } => !matches!(
                            state,
                            ToolState::Error { metadata: Some(md), .. } if md["interrupted"] == true
                        ),
                        _ => false,
                    })
                })
                .unwrap_or(false);
            let finished = a
                .finish
                .as_deref()
                .is_some_and(|f| f != "tool-calls" && f != "unknown");
            if finished && !has_tool_calls && a.parent_id == last_user.id {
                break;
            }
        }

        step += 1;
        let session = engine.sessions.get(&session_id).await?;
        if step == 1 {
            let e = engine.clone();
            let s = session.clone();
            let m = last_user.model.clone();
            let h = msgs.clone();
            tokio::spawn(async move {
                if let Err(err) = generate_title(e, s, m, h).await {
                    tracing::debug!("title generation failed: {err}");
                }
            });
        }

        let model = engine
            .registry()
            .get(&last_user.model.provider_id, &last_user.model.model_id)
            .cloned()
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "model not found: {}/{}",
                    last_user.model.provider_id,
                    last_user.model.model_id
                )
            })?;
        // pool: turn `lunar/*` into a concrete model for this step
        let need = crate::provider::router::Need {
            tools: true,
            vision: msgs.iter().any(|m| {
                m.parts
                    .iter()
                    .any(|p| matches!(&p.kind, PartKind::File { mime, .. } if mime.starts_with("image/")))
            }),
            tokens: msgs
                .iter()
                .flat_map(|m| m.parts.iter())
                .map(|p| match &p.kind {
                    PartKind::Text { text, .. } | PartKind::Reasoning { text, .. } => text.len() as u64,
                    PartKind::Tool { state, .. } => match state {
                        ToolState::Completed { output, .. } => output.len() as u64 + 200,
                        _ => 200,
                    },
                    _ => 50,
                })
                .sum::<u64>()
                / 4,
            user_text: msgs
                .iter()
                .rev()
                .find(|m| matches!(m.info, Message::User(_)))
                .map(|m| {
                    m.parts
                        .iter()
                        .filter_map(|p| match &p.kind {
                            PartKind::Text {
                                text,
                                synthetic: false,
                                ..
                            } => Some(text.as_str()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default(),
        };
        let user_text = need.user_text.clone();
        let (model, route) = match engine.route_model(&model, need, &session_id) {
            Ok(v) => v,
            Err(message) => {
                let err = MessageError::Unknown { message, r#ref: None };
                engine.bus.publish(Event::SessionError {
                    session_id: Some(session_id.clone()),
                    error: err,
                });
                break;
            }
        };

        if let Some(task) = tasks.last() {
            match &task.kind {
                PartKind::Subtask { .. } => {
                    super::subtask::handle(
                        engine.clone(),
                        &session_id,
                        &last_user,
                        task,
                        &model,
                        cancel.clone(),
                    )
                    .await?;
                    continue;
                }
                PartKind::Compaction { auto, overflow, .. } => {
                    let outcome = super::compaction::process(
                        engine.clone(),
                        &session_id,
                        &last_user,
                        &msgs,
                        *auto,
                        overflow.unwrap_or(false),
                        cancel.clone(),
                    )
                    .await?;
                    if outcome == super::compaction::Outcome::Stop {
                        break;
                    }
                    continue;
                }
                _ => {}
            }
        }

        if let Some(f) = &last_finished
            && f.summary != Some(true)
            && is_overflow(&engine.config(), &f.tokens, &model)
        {
            super::compaction::create(&engine, &session_id, &last_user, true, false).await?;
            continue;
        }

        let agents = engine.agents();
        let Some(agent) = agents.get(&last_user.agent).cloned() else {
            let available: Vec<String> = agents
                .list()
                .iter()
                .filter(|a| !a.hidden)
                .map(|a| a.name.clone())
                .collect();
            let message = format!(
                "Agent not found: \"{}\". Available agents: {}",
                last_user.agent,
                available.join(", ")
            );
            engine.bus.publish(Event::SessionError {
                session_id: Some(session_id.clone()),
                error: MessageError::Unknown {
                    message: message.clone(),
                    r#ref: None,
                },
            });
            anyhow::bail!(message);
        };
        let agent = Arc::new(agent);
        let is_last_step = agent.steps.is_some_and(|max| step >= max);
        msgs = super::reminders::apply(&engine, msgs, &agent, &session).await;

        let assistant = AssistantMessage {
            id: ids::ascending(Prefix::Message),
            session_id: session_id.clone(),
            time: AssistantTime {
                created: now_ms(),
                completed: None,
            },
            error: None,
            parent_id: last_user.id.clone(),
            model_id: model.id.clone(),
            provider_id: model.provider_id.clone(),
            mode: agent.name.clone(),
            agent: agent.name.clone(),
            path: MessagePath {
                cwd: engine.directory.display().to_string(),
                root: engine.project.worktree.display().to_string(),
            },
            summary: None,
            cost: 0.0,
            tokens: Tokens::default(),
            structured: None,
            variant: last_user.model.variant.clone(),
            finish: None,
        };
        engine
            .sessions
            .update_message(Message::Assistant(assistant.clone()))
            .await?;

        // tools
        let mut ruleset = agent.permission.clone();
        if let Some(extra) = &session.permission {
            ruleset.extend(extra.iter().cloned());
        }
        let bypass_agent_check = msgs
            .iter()
            .rev()
            .find(|m| matches!(m.info, Message::User(_)))
            .is_some_and(|m| m.parts.iter().any(|p| matches!(p.kind, PartKind::Agent { .. })));
        let mut tools = engine
            .tools
            .resolve(&model, &ruleset, !is_last_step, &agents, &agent);
        // smart.mcp: send an MCP server's tools only when the prompt (or this
        // session's history) relates to it; the rest are named in one line
        let mut mcp_note: Option<String> = None;
        {
            let smart = engine.config().smart.clone().unwrap_or_default();
            if smart.mcp.unwrap_or(true) {
                let index = engine.mcp.index().await;
                if !index.is_empty() {
                    let q = crate::relevance::tokens(&user_text);
                    let used: std::collections::HashSet<String> = msgs
                        .iter()
                        .flat_map(|m| m.parts.iter())
                        .filter_map(|p| match &p.kind {
                            PartKind::Tool { tool, .. } => Some(tool.clone()),
                            _ => None,
                        })
                        .collect();
                    let always = smart.mcp_always.clone().unwrap_or_default();
                    let mut drop_ids: Vec<String> = Vec::new();
                    let mut skipped: Vec<String> = Vec::new();
                    for (name, ids, text) in &index {
                        let mentioned = q.contains(&name.to_lowercase())
                            || user_text.to_lowercase().contains(&name.to_lowercase());
                        let relevant = mentioned
                            || always.iter().any(|a| a == name)
                            || ids.iter().any(|id| used.contains(id))
                            || crate::relevance::score(&q, text) >= 0.35;
                        if !relevant {
                            drop_ids.extend(ids.iter().cloned());
                            skipped.push(format!("{name} ({} tools)", ids.len()));
                        }
                    }
                    if !drop_ids.is_empty() {
                        tools.retain(|t| !drop_ids.contains(&t.def.name));
                        mcp_note = Some(format!(
                            "MCP servers not loaded for this request (name one to use it): {}",
                            skipped.join(", ")
                        ));
                    }
                }
            }
        }
        let json_schema = match &last_user.format {
            Some(OutputFormat::JsonSchema { schema, .. }) => Some(schema.clone()),
            _ => None,
        };
        if let Some(schema) = &json_schema {
            tools.push(crate::tool::registry::ResolvedTool {
                tool: Arc::new(crate::tool::builtins::invalid::InvalidTool),
                def: ToolDef {
                    name: "StructuredOutput".into(),
                    description: "Call this tool exactly once with the final structured result.".into(),
                    input_schema: crate::provider::transform::sanitize_schema(schema),
                },
            });
        }

        // system prompt
        let mut system: Vec<SystemBlock> = Vec::new();
        let base = agent
            .prompt
            .clone()
            .unwrap_or_else(|| system::base_prompt(&model));
        system.push(SystemBlock { text: base });
        let mut rest: Vec<String> = vec![system::environment(system::EnvInput {
            model: &model,
            directory: &engine.directory,
            worktree: &engine.project.worktree,
            is_git: engine.project.vcs.is_some(),
        })];
        {
            let cfg = engine.config();
            let pm = cfg.project_map.clone().unwrap_or_default();
            if pm.enabled.unwrap_or(true) {
                rest.push(
                    engine
                        .project_map
                        .get(&engine.project.worktree, pm.max_chars.unwrap_or(1500)),
                );
            }
        }
        rest.extend(engine.instructions().await);
        if let Some(mcp) = engine.mcp_instructions(&ruleset).await {
            rest.push(mcp);
        }
        if let Some(note) = mcp_note {
            rest.push(note);
        }
        if let Some(skills) = engine.skills_prompt(&agent, &user_text).await {
            rest.push(skills);
        }
        if let Some(extra) = &last_user.system {
            rest.push(extra.clone());
        }
        if json_schema.is_some() {
            rest.push(system::STRUCTURED_OUTPUT_PROMPT.into());
        }
        system.push(SystemBlock {
            text: rest.join("\n\n"),
        });

        let mut model_msgs = history::to_llm_messages(
            &msgs,
            &ToModelOptions {
                current_model: (model.provider_id.clone(), model.id.clone()),
                media_in_tool_results: model.npm == "@ai-sdk/openai",
                tool_output_max_chars: None,
            },
        );
        if is_last_step {
            model_msgs.push(LlmMessage::Assistant {
                content: vec![ContentPart::Text {
                    text: system::MAX_STEPS_PROMPT.into(),
                }],
            });
        }

        let history_arc = Arc::new(msgs);
        let result = processor::process(ProcessInput {
            engine: engine.clone(),
            session_id: session_id.clone(),
            assistant,
            model: model.clone(),
            agent: agent.clone(),
            system,
            messages: model_msgs,
            tools,
            tool_choice: if json_schema.is_some() {
                Some(ToolChoice::Required)
            } else {
                None
            },
            response_format: None,
            variant: last_user.model.variant.clone(),
            cancel: cancel.clone(),
            history: history_arc,
            bypass_agent_check,
            route,
        })
        .await;

        let mut message = result.message;
        if let Some(s) = result.structured {
            structured = Some(s.clone());
            message.structured = Some(s);
            if message.finish.is_none() {
                message.finish = Some("stop".into());
            }
            engine
                .sessions
                .update_message(Message::Assistant(message))
                .await?;
            break;
        }
        let finished = message
            .finish
            .as_deref()
            .is_some_and(|f| f != "tool-calls" && f != "unknown");
        if finished && message.error.is_none() {
            if message.finish.as_deref() == Some("content-filter") {
                let err = MessageError::ContentFilter {
                    message: "The response was blocked by the provider's content filter".into(),
                };
                message.error = Some(err.clone());
                engine
                    .sessions
                    .update_message(Message::Assistant(message))
                    .await?;
                engine.bus.publish(Event::SessionError {
                    session_id: Some(session_id.clone()),
                    error: err,
                });
                break;
            }
            if json_schema.is_some() {
                message.error = Some(MessageError::StructuredOutput {
                    message: "Model did not produce structured output".into(),
                    retries: 0,
                });
                engine
                    .sessions
                    .update_message(Message::Assistant(message))
                    .await?;
                break;
            }
        }
        match result.outcome {
            StepOutcome::Stop => break,
            StepOutcome::Compact => {
                let overflow = message.finish.is_none();
                super::compaction::create(&engine, &session_id, &last_user, true, overflow).await?;
            }
            StepOutcome::Continue => {}
        }
    }
    let _ = structured;
    {
        let e = engine.clone();
        let s = session_id.clone();
        tokio::spawn(async move {
            let _ = super::compaction::prune(&e, &s).await;
        });
    }
    Ok(())
}

/// Generate a short session title from the first exchange using the small model.
async fn generate_title(
    engine: Arc<Engine>,
    session: SessionInfo,
    model_ref: ModelRef,
    history: Vec<MessageWithParts>,
) -> anyhow::Result<()> {
    if session.parent_id.is_some() || !session.title.starts_with("New session") {
        return Ok(());
    }
    let agents = engine.agents();
    let Some(agent) = agents.get("title") else {
        return Ok(());
    };
    let base = engine
        .registry()
        .get(&model_ref.provider_id, &model_ref.model_id)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("model not found"))?;
    let model = engine.registry().small_model(&engine.config(), &base);
    let model = if crate::provider::pool::is_virtual(&model) {
        let need = crate::provider::router::Need {
            tools: false,
            ..Default::default()
        };
        engine
            .router
            .pick(
                &engine.registry(),
                crate::provider::pool::Strategy::Fast,
                &need,
                "title",
                &[],
                0,
            )
            .map(|p| p.model)
            .ok_or_else(|| anyhow::anyhow!("no free model available for title"))?
    } else {
        model
    };
    let (protocol, endpoint) = engine
        .registry()
        .endpoint(&model)
        .map_err(|e| anyhow::anyhow!(e))?;
    let user_text: String = history
        .iter()
        .filter(|m| matches!(m.info, Message::User(_)))
        .flat_map(|m| m.parts.iter())
        .filter_map(|p| match &p.kind {
            PartKind::Text {
                text,
                synthetic: false,
                ..
            } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    if user_text.trim().is_empty() {
        return Ok(());
    }
    let req = LlmRequest {
        model_id: model.api_id.clone(),
        system: vec![SystemBlock {
            text: agent.prompt.clone().unwrap_or_default(),
        }],
        messages: vec![LlmMessage::User {
            content: vec![ContentPart::Text {
                text: user_text.chars().take(4000).collect(),
            }],
        }],
        generation: Generation {
            max_tokens: Some(200),
            temperature: agent.temperature,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut rx = engine
        .registry()
        .client()
        .stream(protocol, endpoint, req, CancellationToken::new());
    let mut title = String::new();
    while let Some(ev) = rx.recv().await {
        match ev {
            Ok(LlmEvent::TextDelta { text, .. }) => title.push_str(&text),
            Err(e) => anyhow::bail!(e),
            _ => {}
        }
    }
    let title = crate::llm::think_tags::strip(&title);
    let title = title
        .trim()
        .trim_start_matches("Title:")
        .trim()
        .trim_matches('"')
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .to_string();
    if title.is_empty() {
        return Ok(());
    }
    let title: String = title.chars().take(100).collect();
    engine
        .sessions
        .modify(&session.id, move |s| s.title = title)
        .await?;
    Ok(())
}
