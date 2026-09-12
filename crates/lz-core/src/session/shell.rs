//! `!` shell mode: run a command directly (no model turn) and record it as a
//! user message + assistant message with a `bash` tool part.

use std::process::Stdio;
use std::sync::Arc;

use lz_schema::ids::{self, Prefix};
use lz_schema::session::*;
use serde_json::json;
use tokio::io::AsyncReadExt;
use tokio_util::sync::CancellationToken;

use crate::engine::Engine;
use crate::storage::now_ms;

pub struct ShellInput {
    pub session_id: String,
    pub command: String,
    pub agent: Option<String>,
    pub model: Option<ModelRef>,
}

pub async fn run(
    engine: Arc<Engine>,
    input: ShellInput,
    cancel: CancellationToken,
) -> anyhow::Result<MessageWithParts> {
    let session = engine.sessions.get(&input.session_id).await?;
    let agents = engine.agents();
    let agent = match &input.agent {
        Some(n) => agents
            .get(n)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Agent not found: \"{n}\""))?,
        None => match &session.agent {
            Some(n) if agents.get(n).is_some() => agents.get(n).unwrap().clone(),
            _ => agents
                .default_agent(engine.config().default_agent.as_deref())
                .clone(),
        },
    };
    let model = input
        .model
        .clone()
        .or_else(|| agent.model.clone())
        .or_else(|| {
            session.model.as_ref().map(|m| ModelRef {
                provider_id: m.provider_id.clone(),
                model_id: m.id.clone(),
                variant: None,
            })
        })
        .or_else(|| {
            engine
                .registry()
                .default_model(&engine.config())
                .map(|m| ModelRef {
                    provider_id: m.provider_id.clone(),
                    model_id: m.id.clone(),
                    variant: None,
                })
        })
        .ok_or_else(|| anyhow::anyhow!("no model configured"))?;

    let user = UserMessage {
        id: ids::ascending(Prefix::Message),
        session_id: input.session_id.clone(),
        time: UserTime { created: now_ms() },
        format: None,
        summary: None,
        agent: agent.name.clone(),
        model: model.clone(),
        system: None,
        tools: None,
    };
    engine
        .sessions
        .update_message(Message::User(user.clone()))
        .await?;
    let up = engine.sessions.new_part(
        &input.session_id,
        &user.id,
        PartKind::Text {
            text: "The following tool was executed by the user".into(),
            synthetic: true,
            ignored: false,
            time: None,
            metadata: None,
        },
    );
    engine.sessions.update_part(up).await?;

    let mut msg = AssistantMessage {
        id: ids::ascending(Prefix::Message),
        session_id: input.session_id.clone(),
        time: AssistantTime {
            created: now_ms(),
            completed: None,
        },
        error: None,
        parent_id: user.id.clone(),
        model_id: model.model_id.clone(),
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
        variant: None,
        finish: None,
    };
    engine
        .sessions
        .update_message(Message::Assistant(msg.clone()))
        .await?;
    let started = now_ms();
    let mut part = engine.sessions.new_part(
        &input.session_id,
        &msg.id,
        PartKind::Tool {
            call_id: ids::ascending(Prefix::Tool),
            tool: "bash".into(),
            state: ToolState::Running {
                input: json!({ "command": input.command }),
                title: None,
                metadata: None,
                time: ToolTimeRunning { start: started },
            },
            metadata: None,
        },
    );
    engine.sessions.update_part(part.clone()).await?;

    let shell = crate::tool::builtins::bash::select_shell(engine.config().shell.as_deref());
    let mut cmd = tokio::process::Command::new(&shell);
    cmd.arg("-c")
        .arg(&input.command)
        .current_dir(&engine.directory)
        .env("TERM", "dumb")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    cmd.process_group(0);
    let mut output = String::new();
    let mut aborted = false;
    match cmd.spawn() {
        Ok(mut child) => {
            #[cfg_attr(not(unix), allow(unused_variables))]
            let pid = child.id();
            let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(64);
            for stream in [
                child
                    .stdout
                    .take()
                    .map(|s| Box::new(s) as Box<dyn tokio::io::AsyncRead + Unpin + Send>),
                child
                    .stderr
                    .take()
                    .map(|s| Box::new(s) as Box<dyn tokio::io::AsyncRead + Unpin + Send>),
            ]
            .into_iter()
            .flatten()
            {
                let tx = tx.clone();
                tokio::spawn(async move {
                    let mut s = stream;
                    let mut buf = vec![0u8; 8192];
                    while let Ok(n) = s.read(&mut buf).await {
                        if n == 0
                            || tx
                                .send(String::from_utf8_lossy(&buf[..n]).to_string())
                                .await
                                .is_err()
                        {
                            break;
                        }
                    }
                });
            }
            drop(tx);
            let mut done = false;
            loop {
                tokio::select! {
                    _ = cancel.cancelled(), if !aborted => {
                        aborted = true;
                        #[cfg(unix)]
                        if let Some(pid) = pid { unsafe { libc::killpg(pid as i32, libc::SIGTERM); } }
                        let _ = child.kill().await;
                    }
                    chunk = rx.recv() => match chunk {
                        None => { if done { break } let _ = child.wait().await; break; }
                        Some(c) => {
                            output.push_str(&c);
                            if let PartKind::Tool { state: ToolState::Running { metadata, .. }, .. } = &mut part.kind {
                                *metadata = Some(json!({ "output": output }));
                            }
                            let _ = engine.sessions.update_part(part.clone()).await;
                        }
                    },
                    _ = child.wait(), if !done => { done = true; }
                }
            }
        }
        Err(e) => output = format!("failed to spawn {shell}: {e}"),
    }
    if aborted {
        output.push_str("\n\n<metadata>\nUser aborted the command\n</metadata>");
    }
    let end = now_ms();
    msg.time.completed = Some(end);
    engine
        .sessions
        .update_message(Message::Assistant(msg.clone()))
        .await?;
    if let PartKind::Tool { state, .. } = &mut part.kind {
        *state = ToolState::Completed {
            input: json!({ "command": input.command }),
            output: output.clone(),
            title: String::new(),
            metadata: json!({ "output": output }),
            time: ToolTimeCompleted {
                start: started,
                end,
                compacted: None,
            },
            attachments: None,
        };
    }
    engine.sessions.update_part(part.clone()).await?;
    Ok(MessageWithParts {
        info: Message::Assistant(msg),
        parts: vec![part],
    })
}
