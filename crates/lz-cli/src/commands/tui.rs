//! Default command: start the engine in-process and run the TUI.

use std::sync::Arc;

use lz_schema::api::EngineApi;

use crate::cli::{TuiArgs, VERSION};
use crate::commands::config::resolve_dir;

pub async fn exec(args: TuiArgs) -> anyhow::Result<i32> {
    let directory = resolve_dir(&args.project)?;
    let engine = lz_core::Engine::start(lz_core::EngineOptions {
        directory,
        auto_approve: args.auto,
        offline: false,
    })
    .await?;
    let paths = engine.paths.clone();
    let config_dirs = engine.config_dirs();
    let tui = lz_core::config::load_tui(&paths, &config_dirs);
    // the web portal runs inside the TUI process and shares the engine
    let web_cfg = tui.extra.get("web").cloned().unwrap_or_default();
    let web_enabled = std::env::var("LZ_WEB")
        .map(|v| v != "0" && v != "false")
        .unwrap_or_else(|_| web_cfg.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true));
    let web_port: u16 = std::env::var("LZ_WEB_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .or_else(|| web_cfg.get("port").and_then(|v| v.as_u64()).map(|v| v as u16))
        .unwrap_or(7411);
    let portal = if web_enabled {
        match lz_web::start(engine.clone(), web_port, VERSION).await {
            Ok(p) => Some(p),
            Err(e) => match lz_web::start(engine.clone(), 0, VERSION).await {
                Ok(p) => {
                    tracing::warn!("port {web_port} busy ({e}); portal on {}", p.url);
                    Some(p)
                }
                Err(e2) => {
                    tracing::warn!("web portal disabled: {e2}");
                    None
                }
            },
        }
    } else {
        None
    };
    let mut theme_dirs: Vec<std::path::PathBuf> = paths
        .global_config_dirs()
        .into_iter()
        .map(|d| d.join("themes"))
        .collect();
    theme_dirs.extend(config_dirs.iter().map(|d| d.join("themes")));
    let opts = lz_tui::TuiOptions {
        session: args.session.clone(),
        continue_last: args.continue_session,
        fork: args.fork,
        prompt: args.prompt.clone(),
        model: args.model.clone(),
        agent: args.agent.clone(),
        tui,
        theme_dirs,
        kv_path: paths.kv(),
        version: VERSION.to_string(),
        web_url: portal.as_ref().map(|p| p.url.clone()),
        mode: if args.auto {
            Some(lz_schema::permission::PermissionMode::Auto)
        } else {
            args.mode
                .as_deref()
                .and_then(lz_schema::permission::PermissionMode::parse)
        },
    };
    let api: Arc<dyn EngineApi> = engine.clone();
    let result = lz_tui::run(api, opts).await;
    drop(portal);
    engine.shutdown().await;
    result?;
    Ok(0)
}
