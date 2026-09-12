//! `task` — run a subagent in a child session and return its final answer.
//! Foreground by default; `background: true` starts the subagent and returns at
//! once — its result is delivered to the parent session as a message when it
//! finishes, and `task_id` collects it explicitly.

use std::borrow::Cow;

use async_trait::async_trait;
use lz_schema::config::AgentMode;
use lz_schema::permission::{Action, Rule};
use lz_schema::session::*;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::agent::Agent;
use crate::permission;
use crate::tool::{Tool, ToolCtx, ToolError, ToolResult, parse_args};

#[derive(Deserialize)]
struct Args {
    description: String,
    prompt: String,
    subagent_type: String,
    #[serde(default)]
    task_id: Option<String>,
    #[serde(default)]
    command: Option<String>,
    #[serde(default)]
    background: Option<bool>,
}

pub struct TaskTool;

/// Child-session ruleset: parent session's deny/external_directory rules plus
/// default denies for `todowrite`/`task` unless the subagent allows them.
pub fn derive_child_permission(parent_session: &[Rule], subagent: &Agent) -> Vec<Rule> {
    let can_task = subagent.permission.iter().any(|r| r.permission == "task");
    let can_todo = subagent.permission.iter().any(|r| r.permission == "todowrite");
    let mut out: Vec<Rule> = parent_session
        .iter()
        .filter(|r| r.permission == "external_directory" || r.action == Action::Deny)
        .cloned()
        .collect();
    if !can_todo {
        out.push(Rule::new("todowrite", "*", Action::Deny));
    }
    if !can_task {
        out.push(Rule::new("task", "*", Action::Deny));
    }
    out
}

/// The child's final answer, or its error.
async fn last_text(engine: &crate::engine::Engine, child_id: &str) -> Result<String, ToolError> {
    let result = crate::session::runner::last_assistant(engine, child_id)
        .await
        .ok_or_else(|| ToolError::Other(format!("Subagent produced no response (task_id: {child_id})")))?;
    if let Message::Assistant(a) = &result.info
        && let Some(err) = &a.error
    {
        return Err(ToolError::Other(format!(
            "Subagent failed (task_id: {child_id}): {}",
            err.message()
        )));
    }
    Ok(result
        .parts
        .iter()
        .rev()
        .find_map(|p| match &p.kind {
            PartKind::Text { text, .. } => Some(text.clone()),
            _ => None,
        })
        .unwrap_or_default())
}

/// Hand a finished background task's answer to the parent session as a
/// message; the parent loop picks it up at its next step, or starts a step
/// if it is idle.
async fn deliver_background_result(
    engine: &crate::engine::Engine,
    parent_id: &str,
    child_id: &str,
    description: &str,
    text: &str,
) -> Result<(), ToolError> {
    use lz_schema::ids::{self, Prefix};
    use lz_schema::session::*;
    let msgs = engine
        .sessions
        .messages(parent_id, None, None)
        .await
        .map_err(ToolError::other)?;
    let Some(last_user) = msgs.iter().rev().find_map(|m| match &m.info {
        Message::User(u) => Some(u.clone()),
        _ => None,
    }) else {
        return Ok(());
    };
    let now = crate::storage::now_ms();
    let cont = UserMessage {
        id: ids::ascending(Prefix::Message),
        session_id: parent_id.into(),
        time: UserTime { created: now },
        format: None,
        summary: None,
        agent: last_user.agent.clone(),
        model: last_user.model.clone(),
        system: None,
        tools: None,
    };
    engine
        .sessions
        .update_message(Message::User(cont.clone()))
        .await
        .map_err(ToolError::other)?;
    let body = format!(
        "Background task \"{description}\" ({child_id}) finished:\n{}\nContinue with what you were doing, using this result where it applies.",
        render(child_id, "completed", text)
    );
    let np = engine.sessions.new_part(
        parent_id,
        &cont.id,
        PartKind::Text {
            text: body,
            synthetic: true,
            ignored: false,
            time: Some(PartTime {
                start: now,
                end: Some(now),
            }),
            metadata: Some(
                json!({ "background_result": { "task_id": child_id, "description": description } }),
            ),
        },
    );
    engine.sessions.update_part(np).await.map_err(ToolError::other)?;
    // an idle parent reacts now; a busy one sees it at its next step
    let eng = engine.self_arc();
    engine.runner.ensure_running(eng, parent_id.to_string());
    Ok(())
}

fn render(session_id: &str, state: &str, text: &str) -> String {
    let tag = if state == "error" {
        "task_error"
    } else {
        "task_result"
    };
    format!("<task id=\"{session_id}\" state=\"{state}\">\n<{tag}>\n{text}\n</{tag}>\n</task>")
}

/// Description with the list of subagents this agent may call.
pub fn describe(agents: &crate::agent::Agents, agent: &Agent) -> String {
    let mut items: Vec<&Agent> = agents
        .list()
        .into_iter()
        .filter(|a| a.mode != AgentMode::Primary)
        .filter(|a| permission::evaluate("task", &a.name, &[&agent.permission]).action != Action::Deny)
        .collect();
    items.sort_by(|a, b| a.name.cmp(&b.name));
    let list = items
        .iter()
        .map(|a| {
            format!(
                "- {}: {}",
                a.name,
                a.description
                    .as_deref()
                    .unwrap_or("This subagent should only be called manually by the user.")
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!("{}\nAgents:\n{list}", crate::tool_description!("task").trim_end())
}

#[async_trait]
impl Tool for TaskTool {
    fn id(&self) -> &'static str {
        "task"
    }
    fn description(&self) -> Cow<'static, str> {
        Cow::Borrowed(crate::tool_description!("task"))
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "description": { "type": "string", "description": "3-5 word label" },
                "prompt": { "type": "string", "description": "Complete instructions" },
                "subagent_type": { "type": "string", "description": "Agent name" },
                "task_id": { "type": "string", "description": "Resume this earlier task, or collect a background one (waits for it)" },
                "background": { "type": "boolean", "description": "Start it and return at once; the result arrives as a message when it finishes. Use for independent work you can run in parallel." }
            },
            "required": ["description", "prompt", "subagent_type"]
        })
    }
    async fn execute(&self, ctx: ToolCtx, args: Value) -> Result<ToolResult, ToolError> {
        let args: Args = parse_args(args)?;
        let engine = ctx.engine.clone();
        let config = engine.config();
        let parent = engine
            .sessions
            .get(&ctx.session_id)
            .await
            .map_err(ToolError::other)?;
        let mut depth = 0;
        let mut current = parent.clone();
        while let Some(pid) = current.parent_id.clone() {
            depth += 1;
            current = engine.sessions.get(&pid).await.map_err(ToolError::other)?;
        }
        if depth >= config.subagent_depth() {
            return Err(ToolError::Invalid(format!(
                "Subagent depth limit reached ({}). Increase \"subagent_depth\" to allow nested subagents.",
                config.subagent_depth()
            )));
        }
        if !ctx.bypass_agent_check {
            ctx.ask(
                "task",
                vec![args.subagent_type.clone()],
                vec!["*".into()],
                json!({ "description": args.description, "subagent_type": args.subagent_type })
                    .as_object()
                    .cloned()
                    .unwrap_or_default(),
            )
            .await?;
        }
        let agents = engine.agents();
        let Some(next) = agents.get(&args.subagent_type).cloned() else {
            return Err(ToolError::Invalid(format!(
                "Unknown agent type: {} is not a valid agent type",
                args.subagent_type
            )));
        };

        let existing = match &args.task_id {
            Some(id) => engine.sessions.get(id).await.ok(),
            None => None,
        };
        // collecting a background task that is still running: just wait for it
        if let Some(s) = &existing
            && engine.runner.is_running(&s.id)
        {
            let child_id = s.id.clone();
            tokio::select! {
                _ = engine.runner.wait(&child_id) => {}
                _ = ctx.cancel.cancelled() => { return Err(ToolError::Aborted); }
            }
            let text = last_text(&engine, &child_id).await?;
            return Ok(ToolResult {
                title: args.description,
                output: render(&child_id, "completed", &text),
                metadata: json!({ "sessionId": child_id, "background": true }),
                attachments: Vec::new(),
            });
        }
        let child = match existing {
            Some(s) => s,
            None => {
                let mut perm = derive_child_permission(parent.permission.as_deref().unwrap_or(&[]), &next);
                if let Some(extra) = config.experimental.as_ref().and_then(|e| e.primary_tools.clone()) {
                    for p in extra {
                        perm.push(Rule::new(p, "*", Action::Deny));
                    }
                }
                engine
                    .sessions
                    .create(
                        Some(ctx.session_id.clone()),
                        Some(format!("{} (@{} subagent)", args.description, next.name)),
                        Some(next.name.clone()),
                        Some(perm),
                    )
                    .await
                    .map_err(ToolError::other)?
            }
        };

        let assistant = engine.sessions.get_message(&ctx.message_id).await.ok();
        let (parent_model, variant) = match assistant {
            Some(Message::Assistant(a)) => (
                ModelRef {
                    provider_id: a.provider_id.clone(),
                    model_id: a.model_id.clone(),
                    variant: None,
                },
                a.variant.clone(),
            ),
            _ => (
                ModelRef {
                    provider_id: String::new(),
                    model_id: String::new(),
                    variant: None,
                },
                None,
            ),
        };
        let model = next.model.clone().unwrap_or(parent_model);
        let metadata = json!({ "parentSessionId": ctx.session_id, "sessionId": child.id, "model": model });
        ctx.report(Some(args.description.clone()), Some(metadata.clone()));

        let req = PromptRequest {
            model: Some(ModelRef {
                variant: if next.model.is_some() { None } else { variant },
                ..model
            }),
            agent: Some(next.name.clone()),
            parts: vec![PartInput::Text {
                id: None,
                text: args.prompt.clone(),
                synthetic: false,
                ignored: false,
            }],
            ..Default::default()
        };
        let _ = args.command;
        let child_id = child.id.clone();
        if args.background == Some(true) {
            crate::session::runner::prompt(engine.clone(), &child_id, req)
                .await
                .map_err(ToolError::other)?;
            engine.runner.track_background(&ctx.session_id, &child_id);
            // deliver the result to the parent when it finishes
            let (eng, parent, cid, desc) = (
                engine.clone(),
                ctx.session_id.clone(),
                child_id.clone(),
                args.description.clone(),
            );
            tokio::spawn(async move {
                eng.runner.wait(&cid).await;
                let text = match last_text(&eng, &cid).await {
                    Ok(t) => t,
                    Err(e) => format!("(failed) {e}"),
                };
                let _ = deliver_background_result(&eng, &parent, &cid, &desc, &text).await;
            });
            return Ok(ToolResult {
                title: format!("{} (background)", args.description),
                output: format!(
                    "Started background task {child_id} (@{}): {}. Keep working; its result will arrive as a message when it finishes. To wait for it explicitly, call task with task_id=\"{child_id}\".",
                    next.name, args.description
                ),
                metadata: json!({ "sessionId": child_id, "parentSessionId": ctx.session_id, "background": true }),
                attachments: Vec::new(),
            });
        }
        // run the child loop; abort it if we're cancelled
        let run = crate::session::runner::prompt(engine.clone(), &child_id, req);
        tokio::pin!(run);
        tokio::select! {
            r = &mut run => { r.map_err(ToolError::other)?; }
            _ = ctx.cancel.cancelled() => { return Err(ToolError::Aborted); }
        }
        tokio::select! {
            _ = engine.runner.wait(&child_id) => {}
            _ = ctx.cancel.cancelled() => {
                engine.runner.abort(&child_id).await;
                return Err(ToolError::Aborted);
            }
        }
        let result = crate::session::runner::last_assistant(&engine, &child_id)
            .await
            .ok_or_else(|| {
                ToolError::Other(format!("Subagent produced no response (task_id: {child_id})"))
            })?;
        if let Message::Assistant(a) = &result.info
            && let Some(err) = &a.error
        {
            return Err(ToolError::Other(format!(
                "Subagent failed (task_id: {child_id}): {}",
                err.message()
            )));
        }
        if let Some(PartKind::Tool {
            state: ToolState::Error { error, .. },
            ..
        }) = result
            .parts
            .iter()
            .rev()
            .find(|p| {
                matches!(
                    p.kind,
                    PartKind::Tool {
                        state: ToolState::Error { .. },
                        ..
                    }
                )
            })
            .map(|p| &p.kind)
        {
            return Err(ToolError::Other(format!(
                "Subagent failed (task_id: {child_id}): {error}"
            )));
        }
        let text = result
            .parts
            .iter()
            .rev()
            .find_map(|p| match &p.kind {
                PartKind::Text { text, .. } => Some(text.clone()),
                _ => None,
            })
            .unwrap_or_default();
        Ok(ToolResult {
            title: args.description,
            output: render(&child_id, "completed", &text),
            metadata,
            attachments: Vec::new(),
        })
    }
}
