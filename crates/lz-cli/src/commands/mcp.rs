//! `lz mcp list|add` — inspect and register MCP servers.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use crate::cli::McpCommand;
use crate::commands::config::resolve_dir;

pub async fn run(cmd: McpCommand) -> anyhow::Result<i32> {
    match cmd {
        McpCommand::List => list().await,
        McpCommand::Add {
            name,
            command,
            url,
            global,
        } => add(name, command, url, global),
    }
}

async fn list() -> anyhow::Result<i32> {
    let directory = resolve_dir(&None)?;
    let engine = lz_core::Engine::start(lz_core::EngineOptions {
        directory,
        auto_approve: false,
        offline: false,
    })
    .await?;
    let config = engine.config();
    let status = engine.mcp.status().await;
    let Some(servers) = &config.mcp else {
        println!("no MCP servers configured");
        engine.shutdown().await;
        return Ok(0);
    };
    let tools = engine.mcp.tools().await;
    for (name, entry) in servers {
        let target = match entry {
            lz_schema::config::McpEntry::Server(lz_schema::config::McpServerConfig::Local {
                command,
                ..
            }) => command.join(" "),
            lz_schema::config::McpEntry::Server(lz_schema::config::McpServerConfig::Remote {
                url, ..
            }) => url.clone(),
            lz_schema::config::McpEntry::Toggle { .. } => "(toggle only)".into(),
        };
        let st = match status.get(name) {
            Some(lz_schema::api::McpStatus::Connected) => "connected".to_string(),
            Some(lz_schema::api::McpStatus::Disabled) => "disabled".to_string(),
            Some(lz_schema::api::McpStatus::Failed { error }) => format!("failed: {error}"),
            Some(lz_schema::api::McpStatus::NeedsAuth) => "needs auth".to_string(),
            None => "unknown".to_string(),
        };
        let prefix = format!("{}_", lz_core::mcp::sanitize(name));
        let n = tools.iter().filter(|t| t.id().starts_with(&prefix)).count();
        println!("{name:<20} {st:<12} {n:>3} tools  {target}");
    }
    engine.shutdown().await;
    Ok(0)
}

fn add(name: String, command: Vec<String>, url: Option<String>, global: bool) -> anyhow::Result<i32> {
    let entry = match (command.is_empty(), url) {
        (false, None) => json!({ "type": "local", "command": command }),
        (true, Some(url)) => json!({ "type": "remote", "url": url }),
        (false, Some(_)) => anyhow::bail!("pass either --command or --url, not both"),
        (true, None) => {
            anyhow::bail!("pass --command <cmd...> for a local server or --url <url> for a remote one")
        }
    };
    let path = target_config(global)?;
    let mut root: Map<String, Value> = match std::fs::read_to_string(&path) {
        Ok(text) => lz_core::config::parse_jsonc(&text, &path)?
            .as_object()
            .cloned()
            .unwrap_or_default(),
        Err(_) => Map::new(),
    };
    let mcp = root.entry("mcp").or_insert_with(|| Value::Object(Map::new()));
    if !mcp.is_object() {
        *mcp = Value::Object(Map::new());
    }
    mcp.as_object_mut().unwrap().insert(name.clone(), entry);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, serde_json::to_string_pretty(&Value::Object(root))? + "\n")?;
    println!("added MCP server `{name}` to {}", path.display());
    Ok(0)
}

/// Existing project/global config file, or `lunarzero.json` where one would go.
fn target_config(global: bool) -> anyhow::Result<PathBuf> {
    let paths = lz_core::Paths::detect();
    if global {
        for name in [
            "lunarzero.jsonc",
            "lunarzero.json",
            "opencode.jsonc",
            "opencode.json",
            "config.json",
        ] {
            let p = paths.config.join(name);
            if p.exists() {
                return Ok(p);
            }
        }
        return Ok(paths.config.join("lunarzero.json"));
    }
    let dir = resolve_dir(&None)?;
    for name in [
        "lunarzero.jsonc",
        "lunarzero.json",
        "opencode.jsonc",
        "opencode.json",
    ] {
        let p = Path::new(&dir).join(name);
        if p.exists() {
            return Ok(p);
        }
    }
    Ok(dir.join("lunarzero.json"))
}
