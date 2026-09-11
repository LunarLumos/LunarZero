//! `lz pool` — the free-tier pool: setup help, model list, quota status.

use lz_core::provider::pool;

use crate::cli::PoolCommand;
use crate::commands::config::resolve_dir;

pub async fn run(cmd: Option<PoolCommand>) -> anyhow::Result<i32> {
    match cmd.unwrap_or(PoolCommand::Status) {
        PoolCommand::Setup => setup().await,
        PoolCommand::List { all } => list(all).await,
        PoolCommand::Status => status().await,
    }
}

async fn engine() -> anyhow::Result<std::sync::Arc<lz_core::Engine>> {
    let directory = resolve_dir(&None)?;
    lz_core::Engine::start(lz_core::EngineOptions {
        directory,
        auto_approve: false,
        offline: true,
    })
    .await
}

async fn setup() -> anyhow::Result<i32> {
    let engine = engine().await?;
    let registry = engine.registry();
    let cat = pool::catalog();
    println!(
        "Free-tier pool: {} models across {} providers (catalog {}).\nAdd a key with `lz auth login <provider>` or export the env var; then use model `lunar/auto`.\n",
        cat.models.len(),
        cat.providers.len(),
        cat.version
    );
    let mut connected = 0;
    for (id, pp) in &cat.providers {
        let is_connected = registry.providers.get(id).is_some_and(|p| p.connected());
        connected += is_connected as usize;
        let n = cat.models.iter().filter(|m| &m.provider == id).count();
        let best = cat
            .models
            .iter()
            .filter(|m| &m.provider == id)
            .map(|m| m.quality)
            .max()
            .unwrap_or(0);
        println!(
            "{} {:<22} {:>3} models  best quality {:>3}  {}",
            if is_connected { "●" } else { "○" },
            id,
            n,
            best,
            pp.env.join(" | ")
        );
        println!("     {}  — {}", pp.signup, pp.note);
    }
    println!(
        "\n{connected}/{} providers connected. `lunar/auto` balances quality, speed and quota; `lunar/smart` and `lunar/fast` force one.",
        cat.providers.len()
    );
    if connected == 0 {
        println!(
            "Tip: Groq, Cerebras, Google AI Studio and OpenRouter keys take a minute each and need no card."
        );
    }
    engine.shutdown().await;
    Ok(0)
}

async fn list(all: bool) -> anyhow::Result<i32> {
    let engine = engine().await?;
    let registry = engine.registry();
    let cat = pool::catalog();
    let usage = engine.router.usage(&registry);
    println!(
        "{:<4} {:<4}   {:<22} {:<48} {:<5} {:<7} limits rpm/rpd/tpm/tpd",
        "qual", "spd", "provider", "model", "tools", "ctx"
    );
    let lim = |v: Option<u64>| v.map(|n| n.to_string()).unwrap_or_else(|| "-".into());
    for m in &cat.models {
        let connected = registry.providers.get(&m.provider).is_some_and(|p| p.connected());
        if !connected && !all {
            continue;
        }
        let info = cat.info(m);
        let cd = usage
            .iter()
            .find(|u| u.provider == m.provider && u.model == m.id)
            .map(|u| u.cooldown_secs)
            .unwrap_or(0);
        println!(
            "{:<4} {:<4} {} {:<22} {:<48} {:<5} {:<7} {}/{}/{}/{}{}",
            m.quality,
            m.speed,
            if connected { "●" } else { "○" },
            m.provider,
            m.id,
            if m.tools { "yes" } else { "no" },
            format!("{}k", m.context as u64 / 1000),
            lim(info.rpm),
            lim(info.rpd),
            lim(info.tpm),
            lim(info.tpd),
            if cd > 0 {
                format!("  ⏸ {cd}s")
            } else {
                String::new()
            }
        );
    }
    if !all {
        println!("\n(connected providers only; `--all` shows the whole pool)");
    }
    engine.shutdown().await;
    Ok(0)
}

async fn status() -> anyhow::Result<i32> {
    let engine = engine().await?;
    let registry = engine.registry();
    let usage = engine.router.usage(&registry);
    if usage.is_empty() {
        println!("no pool provider connected — run `lz pool setup`");
        engine.shutdown().await;
        return Ok(0);
    }
    println!(
        "{:<22} {:<44} {:>7} {:>8} {:>9} {:>9} {:>8} {:>5}  state",
        "provider", "model", "rpm", "rpd", "tpm", "tpd", "ttft", "tok/s"
    );
    for u in &usage {
        let m = registry.get(&u.provider, &u.model);
        let f = m.and_then(|m| m.pool.clone()).unwrap_or_default();
        let fmt = |used: u64, limit: Option<u64>| match limit {
            Some(l) => format!("{used}/{l}"),
            None => format!("{used}"),
        };
        let state = if u.cooldown_secs > 0 {
            format!(
                "⏸ {}s  {}",
                u.cooldown_secs,
                u.last_error
                    .lines()
                    .next()
                    .unwrap_or("")
                    .chars()
                    .take(60)
                    .collect::<String>()
            )
        } else {
            "ready".into()
        };
        println!(
            "{:<22} {:<44} {:>7} {:>8} {:>9} {:>9} {:>8} {:>5}  {}",
            u.provider,
            u.model,
            fmt(u.rpm_used, f.rpm),
            fmt(u.rpd_used, f.rpd),
            fmt(u.tpm_used, f.tpm),
            fmt(u.tpd_used, f.tpd),
            if u.ttft_ms > 0 {
                format!("{}ms", u.ttft_ms)
            } else {
                "-".into()
            },
            if u.tps > 0 { u.tps.to_string() } else { "-".into() },
            state
        );
    }
    println!("\nledger: {}", engine.paths.state.join("quota.json").display());
    engine.shutdown().await;
    Ok(0)
}
