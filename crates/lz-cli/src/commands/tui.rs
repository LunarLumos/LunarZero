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
    };
    let api: Arc<dyn EngineApi> = engine.clone();
    let result = lz_tui::run(api, opts).await;
    engine.shutdown().await;
    result?;
    Ok(0)
}
