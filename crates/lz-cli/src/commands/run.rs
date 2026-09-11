//! `lz run` — non-interactive prompt execution. Text mode streams the
//! assistant's text to stdout and tool activity to stderr; `--format json`
//! emits one JSON event per line.

use std::io::{IsTerminal, Read, Write};
use std::sync::Arc;

use futures::StreamExt;
use lz_schema::session::*;
use lz_schema::{EngineApi, Event};

use crate::cli::{RunArgs, RunFormat};
use crate::commands::config::resolve_dir;

pub async fn exec(args: RunArgs) -> anyhow::Result<i32> {
    let mut message = args.message.join(" ");
    if !std::io::stdin().is_terminal() {
        let mut piped = String::new();
        std::io::stdin().read_to_string(&mut piped)?;
        if !piped.trim().is_empty() {
            if !message.is_empty() {
                message.push('\n');
            }
            message.push_str(piped.trim_end());
        }
    }
    if message.trim().is_empty() && args.command.is_none() {
        anyhow::bail!("no prompt given (pass a message or pipe one on stdin)");
    }
    if let Some(c) = &mut args.command.clone() {
        *c = c.trim_start_matches('/').to_string();
    }

    let directory = resolve_dir(&args.dir)?;
    let engine = lz_core::Engine::start(lz_core::EngineOptions {
        directory,
        auto_approve: args.auto,
        offline: false,
    })
    .await?;

    // session
    let session = if let Some(id) = &args.session {
        engine.get_session(id).await?
    } else if args.continue_session {
        let mut list = engine
            .list_sessions(lz_schema::api::SessionQuery {
                limit: Some(1),
                roots: true,
                ..Default::default()
            })
            .await?;
        match list.pop() {
            Some(s) => s,
            None => engine.create_session(Default::default()).await?,
        }
    } else {
        engine
            .create_session(lz_schema::api::CreateSession {
                title: args.title.clone(),
                ..Default::default()
            })
            .await?
    };

    let model = match &args.model {
        Some(spec) => {
            let m = engine
                .registry()
                .resolve(spec)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("unknown model: {spec}"))?;
            Some(ModelRef {
                provider_id: m.provider_id,
                model_id: m.id,
                variant: args.variant.clone(),
            })
        }
        None => None,
    };

    let mut parts: Vec<PartInput> = Vec::new();
    for f in &args.file {
        let abs = engine.resolve_path(&f.display().to_string());
        parts.push(PartInput::File {
            id: None,
            mime: "text/plain".into(),
            filename: abs.file_name().map(|n| n.to_string_lossy().to_string()),
            url: format!("file://{}", abs.display()),
            source: None,
        });
    }
    if !message.trim().is_empty() {
        // `@path` mentions become file parts, `@agent` become agent parts
        parts.extend(lz_core::command::resolve_parts(&engine, &message));
    }

    let format = args.format;
    let events = engine.subscribe();
    let printer = tokio::spawn(print_events(events, session.id.clone(), format, engine.clone()));

    let req = PromptRequest {
        model,
        agent: args.agent.clone(),
        variant: args.variant.clone(),
        parts,
        ..Default::default()
    };
    let session_id = session.id.clone();
    let result = match &args.command {
        Some(cmd) => {
            let r = lz_core::command::execute(
                engine.clone(),
                lz_core::command::CommandInput {
                    session_id: session_id.clone(),
                    command: cmd.clone(),
                    arguments: req
                        .parts
                        .iter()
                        .filter_map(|p| match p {
                            PartInput::Text { text, .. } => Some(text.clone()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join(" "),
                    agent: req.agent.clone(),
                    model: req.model.clone(),
                    variant: req.variant.clone(),
                    parts: req
                        .parts
                        .iter()
                        .filter(|p| !matches!(p, PartInput::Text { .. }))
                        .cloned()
                        .collect(),
                },
            )
            .await;
            match r {
                Ok(_) => {
                    engine.runner.wait(&session_id).await;
                    lz_core::session::runner::last_assistant(&engine, &session_id)
                        .await
                        .ok_or_else(|| lz_schema::ApiError::not_found("no assistant message"))
                }
                Err(e) => Err(lz_schema::ApiError::invalid(e)),
            }
        }
        None => engine.prompt(&session_id, req).await,
    };

    // let the printer drain
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    printer.abort();
    let _ = printer.await;

    let code = match result {
        Ok(msg) => match &msg.info {
            Message::Assistant(a) if a.error.is_some() => {
                if format == RunFormat::Text {
                    eprintln!("\nerror: {}", a.error.as_ref().unwrap().message());
                }
                1
            }
            _ => 0,
        },
        Err(e) => {
            eprintln!("error: {e}");
            1
        }
    };
    if format == RunFormat::Text {
        println!();
    }
    engine.shutdown().await;
    Ok(code)
}

async fn print_events(
    mut events: futures::stream::BoxStream<'static, Event>,
    session_id: String,
    format: RunFormat,
    engine: Arc<lz_core::Engine>,
) {
    let mut stdout = std::io::stdout();
    let mut in_text = false;
    let mut in_reasoning = false;
    while let Some(ev) = events.next().await {
        if ev.session_id().is_some_and(|s| s != session_id) {
            continue;
        }
        match format {
            RunFormat::Json => {
                if let Ok(line) = serde_json::to_string(&ev) {
                    let _ = writeln!(stdout, "{line}");
                    let _ = stdout.flush();
                }
            }
            RunFormat::Text => match &ev {
                Event::PartDelta {
                    field,
                    delta,
                    part_id,
                    ..
                } if field == "text" => {
                    // reasoning deltas share the field name; distinguish by part id lookup is costly,
                    // so track via part.updated events below.
                    let _ = part_id;
                    if in_reasoning {
                        continue;
                    }
                    in_text = true;
                    let _ = write!(stdout, "{delta}");
                    let _ = stdout.flush();
                }
                Event::PartUpdated { part, .. } => match &part.kind {
                    PartKind::Reasoning { time, .. } => in_reasoning = time.end.is_none(),
                    PartKind::Tool { tool, state, .. } => match state {
                        ToolState::Running { title, .. } => {
                            if in_text {
                                let _ = writeln!(stdout);
                                in_text = false;
                            }
                            eprintln!("⚙ {} {}", tool, title.as_deref().unwrap_or(""));
                        }
                        ToolState::Completed { title, output, .. } => {
                            let preview: String = output.lines().take(3).collect::<Vec<_>>().join(" | ");
                            eprintln!(
                                "✓ {tool} {title}{}",
                                if preview.is_empty() {
                                    String::new()
                                } else {
                                    format!(" — {}", preview.chars().take(120).collect::<String>())
                                }
                            );
                        }
                        ToolState::Error { error, .. } => eprintln!("✗ {tool}: {error}"),
                        _ => {}
                    },
                    _ => {}
                },
                Event::PermissionAsked(req) => {
                    if in_text {
                        let _ = writeln!(stdout);
                        in_text = false;
                    }
                    let reply = ask_on_terminal(req);
                    let _ = engine
                        .reply_permission(
                            &req.id,
                            lz_schema::api::PermissionReplyRequest { reply, message: None },
                        )
                        .await;
                }
                Event::SessionStatus {
                    status: SessionStatus::Retry { attempt, message, .. },
                    ..
                } => {
                    eprintln!("↻ retry {attempt}: {message}");
                }
                Event::SessionError { error, .. } => {
                    eprintln!("error: {}", error.message());
                }
                _ => {}
            },
        }
    }
}

fn ask_on_terminal(req: &PermissionRequest) -> PermissionReply {
    if !std::io::stdin().is_terminal() {
        eprintln!(
            "permission '{}' for {:?} auto-rejected (no TTY; use --auto to approve)",
            req.permission, req.patterns
        );
        return PermissionReply::Reject;
    }
    eprintln!("\n┌ permission: {} → {}", req.permission, req.patterns.join(", "));
    if let Some(diff) = req.metadata.get("diff").and_then(|d| d.as_str()) {
        for line in diff.lines().take(40) {
            eprintln!("│ {line}");
        }
    }
    eprint!("└ [y]es once / [a]lways / [n]o: ");
    let mut line = String::new();
    let _ = std::io::stdin().read_line(&mut line);
    match line.trim().to_lowercase().as_str() {
        "y" | "yes" | "" => PermissionReply::Once,
        "a" | "always" => PermissionReply::Always,
        _ => PermissionReply::Reject,
    }
}
