//! `task` — run a subagent in a child session and return its final answer.
//! Foreground mode only; background subagents are deferred.

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
    format!(
        "{}\nAvailable agent types and the tools they have access to:\n{list}",
        crate::tool_description!("task")
    )
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
                "description": { "type": "string", "description": "A short (3-5 words) description of the task" },
                "prompt": { "type": "string", "description": "The task for the agent to perform" },
                "subagent_type": { "type": "string", "description": "The type of specialized agent to use for this task" },
                "task_id": { "type": "string", "description": "This should only be set if you mean to resume a previous task (you can pass a prior task_id and the task will continue the same subagent session as before instead of creating a fresh one)" },
                "command": { "type": "string", "description": "The command that triggered this task" }
            },
            "required": ["description", "prompt", "subagent_type"]
        })
    }
    async fn execute(&self, ctx: ToolCtx, args: Value) -> Result<ToolResult, ToolError> {
        let args: Args = parse_args(args)?;
        if args.background == Some(true) {
            return Err(ToolError::Invalid(
                "Background subagents are not supported yet; run the task in the foreground.".into(),
            ));
        }
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
