//! `lz session list|delete`, `lz export`, `lz import`.

use std::path::PathBuf;

use lz_schema::EngineApi;
use lz_schema::session::*;

use crate::cli::SessionCommand;
use crate::commands::config::resolve_dir;

async fn engine() -> anyhow::Result<std::sync::Arc<lz_core::Engine>> {
    let directory = resolve_dir(&None)?;
    lz_core::Engine::start(lz_core::EngineOptions {
        directory,
        auto_approve: false,
        offline: true,
    })
    .await
}

fn fmt_time(ms: u64) -> String {
    jiff::Timestamp::from_millisecond(ms as i64)
        .map(|t| {
            t.to_zoned(jiff::tz::TimeZone::system())
                .strftime("%Y-%m-%d %H:%M")
                .to_string()
        })
        .unwrap_or_default()
}

pub async fn run(cmd: SessionCommand) -> anyhow::Result<i32> {
    let engine = engine().await?;
    match cmd {
        SessionCommand::List { limit, json } => {
            let sessions = engine
                .list_sessions(lz_schema::api::SessionQuery {
                    limit: Some(limit),
                    roots: true,
                    ..Default::default()
                })
                .await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&sessions)?);
                return Ok(0);
            }
            if sessions.is_empty() {
                println!("no sessions in this project");
                return Ok(0);
            }
            for s in sessions {
                let model = s
                    .model
                    .as_ref()
                    .map(|m| format!("{}/{}", m.provider_id, m.id))
                    .unwrap_or_default();
                println!(
                    "{}  {}  {:<40}  {}",
                    s.id,
                    fmt_time(s.time.updated),
                    truncate(&s.title, 40),
                    model
                );
            }
            Ok(0)
        }
        SessionCommand::Delete { id } => {
            engine.delete_session(&id).await?;
            println!("deleted {id}");
            Ok(0)
        }
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(n - 1).collect::<String>())
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Export {
    info: SessionInfo,
    messages: Vec<MessageWithParts>,
}

pub async fn export(session: String, output: Option<PathBuf>) -> anyhow::Result<i32> {
    let engine = engine().await?;
    let info = engine.get_session(&session).await?;
    let messages = engine.messages(&session, Default::default()).await?;
    let text = serde_json::to_string_pretty(&Export { info, messages })?;
    match output {
        Some(p) => {
            std::fs::write(&p, text)?;
            println!("exported to {}", p.display());
        }
        None => println!("{text}"),
    }
    Ok(0)
}

pub async fn import(file: PathBuf) -> anyhow::Result<i32> {
    let engine = engine().await?;
    let text = std::fs::read_to_string(&file)?;
    let data: Export = serde_json::from_str(&text)?;
    let mut info = data.info;
    info.project_id = engine.project.id.clone();
    info.directory = engine.directory.display().to_string();
    let id = info.id.clone();
    engine.sessions.update(info).await?;
    for m in data.messages {
        engine.sessions.update_message(m.info).await?;
        for p in m.parts {
            engine.sessions.update_part(p).await?;
        }
    }
    println!("imported session {id}");
    Ok(0)
}
