//! `@agent` mentions in a user prompt become `subtask` parts; the loop runs
//! them through the `task` tool in a synthetic assistant turn.

use std::sync::Arc;

use lz_schema::ids::{self, Prefix};
use lz_schema::session::*;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use crate::engine::Engine;
use crate::provider::Model;
use crate::storage::now_ms;
use crate::tool::{ToolCtx, ToolProgress};

pub async fn handle(
    engine: Arc<Engine>,
    session_id: &str,
    last_user: &UserMessage,
    task: &Part,
    model: &Model,
    cancel: CancellationToken,
) -> anyhow::Result<()> {
    let PartKind::Subtask {
        prompt,
        description,
        agent: agent_name,
        model: task_model,
        command,
    } = &task.kind
    else {
        return Ok(());
    };
    let task_model = task_model
        .as_ref()
        .and_then(|m| engine.registry().get(&m.provider_id, &m.model_id).cloned())
        .unwrap_or_else(|| model.clone());
    let assistant = AssistantMessage {
        id: ids::ascending(Prefix::Message),
        session_id: session_id.into(),
        time: AssistantTime {
            created: now_ms(),
            completed: None,
        },
        error: None,
        parent_id: last_user.id.clone(),
        model_id: task_model.id.clone(),
        provider_id: task_model.provider_id.clone(),
        mode: agent_name.clone(),
        agent: agent_name.clone(),
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
    let call_id = ids::ascending(Prefix::Tool);
    let input = json!({
        "prompt": prompt, "description": description, "subagent_type": agent_name, "command": command
    });
    let mut part = engine.sessions.new_part(
        session_id,
        &assistant.id,
        PartKind::Tool {
            call_id: call_id.clone(),
            tool: "task".into(),
            state: ToolState::Running {
                input: input.clone(),
                title: None,
                metadata: None,
                time: ToolTimeRunning { start: now_ms() },
            },
            metadata: None,
        },
    );
    engine.sessions.update_part(part.clone()).await?;

    let agents = engine.agents();
    let Some(agent) = agents.get(agent_name).cloned() else {
        anyhow::bail!("Agent not found: \"{agent_name}\"");
    };
    let Some(tool) = engine.tools.get("task") else {
        anyhow::bail!("task tool unavailable");
    };
    let (ptx, mut prx) = tokio::sync::mpsc::unbounded_channel::<ToolProgress>();
    let ctx = ToolCtx {
        session_id: session_id.into(),
        message_id: assistant.id.clone(),
        call_id: call_id.clone(),
        agent: Arc::new(agent),
        cancel: cancel.child_token(),
        engine: engine.clone(),
        progress: ptx,
        messages: Arc::new(Vec::new()),
        bypass_agent_check: true,
    };
    let exec = tool.execute(ctx, input.clone());
    tokio::pin!(exec);
    let result = loop {
        tokio::select! {
            r = &mut exec => break r,
            Some(p) = prx.recv() => {
                if let PartKind::Tool { state: ToolState::Running { title, metadata, .. }, .. } = &mut part.kind {
                    if p.title.is_some() { *title = p.title; }
                    if p.metadata.is_some() { *metadata = p.metadata; }
                }
                let _ = engine.sessions.update_part(part.clone()).await;
            }
        }
    };
    let start = match &part.kind {
        PartKind::Tool {
            state: ToolState::Running { time, .. },
            ..
        } => time.start,
        _ => now_ms(),
    };
    if let PartKind::Tool { state, .. } = &mut part.kind {
        *state = match result {
            Ok(r) => ToolState::Completed {
                input: input.clone(),
                output: r.output,
                title: r.title,
                metadata: r.metadata,
                time: ToolTimeCompleted {
                    start,
                    end: now_ms(),
                    compacted: None,
                },
                attachments: if r.attachments.is_empty() {
                    None
                } else {
                    Some(r.attachments)
                },
            },
            Err(e) => ToolState::Error {
                input: input.clone(),
                error: e.to_string(),
                metadata: None,
                time: ToolTimeError { start, end: now_ms() },
            },
        };
    }
    engine.sessions.update_part(part).await?;
    let mut assistant = assistant;
    assistant.finish = Some("tool-calls".into());
    assistant.time.completed = Some(now_ms());
    engine
        .sessions
        .update_message(Message::Assistant(assistant))
        .await?;
    Ok(())
}
