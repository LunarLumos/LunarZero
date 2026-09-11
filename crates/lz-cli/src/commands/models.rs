//! `lz models` — list models from connected providers.

use crate::commands::config::resolve_dir;

pub async fn run(provider: Option<String>, refresh: bool) -> anyhow::Result<i32> {
    let directory = resolve_dir(&None)?;
    let engine = lz_core::Engine::start(lz_core::EngineOptions {
        directory,
        auto_approve: false,
        offline: false,
    })
    .await?;
    if refresh {
        engine.refresh_catalog().await?;
    }
    let config = engine.config();
    // an explicit provider may be unconnected: its catalog models are not kept by default
    let registry = match &provider {
        Some(_) => std::sync::Arc::new(lz_core::provider::load_all(&engine.paths, &config, false).await),
        None => engine.registry(),
    };
    let default = registry.default_model(&config).map(|m| m.full_id());
    let mut any = false;
    for p in registry.providers.values() {
        if let Some(filter) = &provider {
            if &p.id != filter {
                continue;
            }
        } else if !p.connected() {
            continue;
        }
        for m in p.models.values() {
            if m.protocol.is_none() && provider.is_none() {
                continue;
            }
            any = true;
            let star = if default.as_deref() == Some(&m.full_id()) {
                "*"
            } else {
                " "
            };
            let unsupported = if m.protocol.is_none() {
                " (unsupported)"
            } else {
                ""
            };
            println!("{star} {}/{}{unsupported}", p.id, m.id);
        }
    }
    if !any {
        eprintln!(
            "no connected providers. Set an API key env var (e.g. OPENAI_API_KEY) or run `lz auth login`."
        );
        eprintln!("Pass a provider id to list its catalog: lz models openai");
    }
    Ok(0)
}
