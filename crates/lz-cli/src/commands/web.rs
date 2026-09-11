//! `lz web` — run the local portal on its own (the TUI starts one too).

use crate::cli::VERSION;
use crate::commands::config::resolve_dir;

pub async fn exec(port: u16, no_open: bool) -> anyhow::Result<i32> {
    let directory = resolve_dir(&None)?;
    let engine = lz_core::Engine::start(lz_core::EngineOptions {
        directory,
        auto_approve: false,
        offline: false,
    })
    .await?;
    lz_web::serve_blocking(engine.clone(), port, VERSION, !no_open).await?;
    engine.shutdown().await;
    Ok(0)
}
