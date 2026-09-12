//! `lz pool` — the free-tier pool: setup help, model list, quota status.

use lz_core::provider::pool;

use crate::cli::PoolCommand;
use crate::commands::config::resolve_dir;

pub async fn run(cmd: Option<PoolCommand>) -> anyhow::Result<i32> {
    match cmd.unwrap_or(PoolCommand::Status) {
        PoolCommand::Setup => setup().await,
        PoolCommand::List { all } => list(all).await,
        PoolCommand::Status => status().await,
        PoolCommand::Why { model } => why(&model).await,
        PoolCommand::Report => report().await,
        PoolCommand::Eval => {
            let (acc, per, wrong) = lz_core::provider::router::routing_eval();
            println!("routing classifier — assets/eval/routing.jsonl");
            for (label, c, t) in &per {
                println!(
                    "  {label:<13} {c:>3}/{t:<3} {:.0}%",
                    *c as f64 / *t as f64 * 100.0
                );
            }
            println!("  overall       {:.1}%", acc * 100.0);
            if !wrong.is_empty() {
                println!("misses:");
                for m in &wrong {
                    println!("  want {:<12} got {:<12} {}", m.want, m.got, m.text);
                }
            }
            Ok(0)
        }
    }
}

async fn why(key: &str) -> anyhow::Result<i32> {
    let engine = engine().await?;
    let registry = engine.registry();
    let need = lz_core::provider::router::Need {
        tools: true,
        tokens: 4_000,
        ..Default::default()
    };
    let Some(w) = engine.router.why(&registry, key, &need) else {
        println!("{key}: not a known model (see `lz pool list --all`)");
        engine.shutdown().await;
        return Ok(1);
    };
    let fmt_win = |(used, cap): (u64, Option<u64>)| match cap {
        Some(c) => format!("{used}/{c}"),
        None => format!("{used}/∞"),
    };
    println!("{}", w.key);
    println!(
        "  pool member      {}",
        if w.in_pool {
            "yes"
        } else {
            "no — not routed by lunar/auto"
        }
    );
    println!(
        "  fits a request   {} (tools, ~4k tokens)",
        if w.fits_request {
            "yes"
        } else {
            "no (missing tool support / context too small / no key)"
        }
    );
    match &w.blocked {
        None => println!("  state            ready"),
        Some((why, wait)) => match wait {
            Some(ms) => println!("  state            BLOCKED — {why}; free in {}s", ms / 1000),
            None => println!("  state            BLOCKED — {why}"),
        },
    }
    if let Some((ms, why)) = &w.provider_cooldown {
        println!("  provider         cooling down {}s: {why}", ms / 1000);
    }
    println!("  quality / speed  {} / {}", w.quality, w.speed);
    println!(
        "  usage            rpm {}  rpd {}  tpm {}  tpd {}",
        fmt_win(w.rpm),
        fmt_win(w.rpd),
        fmt_win(w.tpm),
        fmt_win(w.tpd)
    );
    println!(
        "  measured         ttft {}  {} tok/s",
        w.ttft_ms.map(|v| format!("{v} ms")).unwrap_or_else(|| "—".into()),
        w.tps.map(|v| v.to_string()).unwrap_or_else(|| "—".into())
    );
    println!("  failures         {}", w.failures);
    if !w.last_error.is_empty() {
        println!("  last error       {}", w.last_error);
    }
    let policy = engine.config().pool.as_ref().and_then(|p| p.policy.clone());
    if let Some(p) = policy {
        let pol = lz_core::provider::router::Policy::from_config(Some(&p));
        if let Some(m) = registry.get(
            key.split('/').next().unwrap_or(""),
            key.split_once('/').map(|x| x.1).unwrap_or(""),
        ) {
            let (adj, tag) = pol.adjust(m);
            if let Some(t) = tag {
                println!("  policy           {t} ({adj:+.2})");
            }
        }
    }
    engine.shutdown().await;
    Ok(0)
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

/// Usage over the last 24 h from the router ledger, with the list-price
/// equivalent from the model catalog (paid rates for the same models).
async fn report() -> anyhow::Result<i32> {
    let engine = engine().await?;
    let registry = engine.registry();
    let catalog = lz_core::provider::catalog::embedded();
    let usage = engine.router.usage(&registry);
    let mut rows: Vec<(String, u64, u64, f64)> = Vec::new();
    let (mut reqs, mut toks, mut cost, mut cooling) = (0u64, 0u64, 0f64, 0usize);
    for u in &usage {
        if u.rpd_used == 0 && u.tpd_used == 0 {
            continue;
        }
        // blended list price: most tokens in a coding session are prompt tokens
        let price = catalog
            .get(&u.provider)
            .and_then(|p| p.models.get(&u.model))
            .and_then(|m| m.cost.as_ref())
            .map(|c| 0.8 * c.input + 0.2 * c.output)
            .unwrap_or(0.0);
        let usd = u.tpd_used as f64 / 1_000_000.0 * price;
        reqs += u.rpd_used;
        toks += u.tpd_used;
        cost += usd;
        cooling += (u.cooldown_secs > 0) as usize;
        rows.push((format!("{}/{}", u.provider, u.model), u.rpd_used, u.tpd_used, usd));
    }
    rows.sort_by(|a, b| b.2.cmp(&a.2));
    println!(
        "last 24 h — {} requests · {} tokens · {} model(s) used",
        reqs,
        toks,
        rows.len()
    );
    println!("{:<48} {:>6} {:>10} {:>9}", "model", "reqs", "tokens", "list $");
    for (k, r, t, usd) in &rows {
        println!("{k:<48} {r:>6} {t:>10} {usd:>9.3}");
    }
    println!(
        "\npaid at list price these tokens would have cost ${cost:.2}; on free tiers they cost $0.\n{} model(s) currently cooling down.",
        cooling
    );
    engine.shutdown().await;
    Ok(0)
}
