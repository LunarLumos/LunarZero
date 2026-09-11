//! `lz web` — a local dashboard (127.0.0.1 only, token-protected) for API
//! keys, the free pool, sessions and configuration.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{Html, IntoResponse, Json};
use axum::routing::{get, put};
use axum::{Router, middleware};
use lz_schema::api::{AuthInfo, EngineApi, SessionQuery};
use serde_json::{Value, json};

use crate::cli::VERSION;
use crate::commands::config::resolve_dir;

const INDEX: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../assets/web/index.html"
));

struct AppState {
    engine: Arc<lz_core::Engine>,
    token: String,
}

type St = State<Arc<AppState>>;

pub async fn exec(port: u16, no_open: bool) -> anyhow::Result<i32> {
    let directory = resolve_dir(&None)?;
    let engine = lz_core::Engine::start(lz_core::EngineOptions {
        directory,
        auto_approve: false,
        offline: false,
    })
    .await?;
    let token: String = {
        use rand::RngCore;
        let mut b = [0u8; 16];
        rand::rng().fill_bytes(&mut b);
        b.iter().map(|x| format!("{x:02x}")).collect()
    };
    let state = Arc::new(AppState {
        engine: engine.clone(),
        token: token.clone(),
    });
    let api = Router::new()
        .route("/status", get(status))
        .route("/providers", get(providers))
        .route("/auth/{provider}", put(auth_set).delete(auth_remove))
        .route("/pool", get(pool))
        .route("/pool/reset", axum::routing::post(pool_reset))
        .route("/sessions", get(sessions))
        .route("/config", get(config))
        .layer(middleware::from_fn_with_state(state.clone(), require_token));
    let app = Router::new()
        .route("/", get(|| async { Html(INDEX) }))
        .nest("/api", api)
        .with_state(state);
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
    let addr = listener.local_addr()?;
    let url = format!("http://{addr}/?token={token}");
    println!("LunarZero dashboard: {url}\n(ctrl+c to stop)");
    if !no_open {
        let opener = if cfg!(target_os = "macos") {
            "open"
        } else {
            "xdg-open"
        };
        let _ = std::process::Command::new(opener)
            .arg(&url)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }
    let serve = axum::serve(listener, app);
    tokio::select! {
        r = serve => { r?; }
        _ = tokio::signal::ctrl_c() => {}
    }
    engine.shutdown().await;
    Ok(0)
}

async fn require_token(
    State(state): St,
    headers: HeaderMap,
    req: axum::extract::Request,
    next: middleware::Next,
) -> axum::response::Response {
    let ok = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .is_some_and(|t| t == state.token);
    if !ok {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "message": "unauthorized" })),
        )
            .into_response();
    }
    next.run(req).await
}

fn err(e: impl std::fmt::Display) -> (StatusCode, Json<Value>) {
    (StatusCode::BAD_REQUEST, Json(json!({ "message": e.to_string() })))
}

fn mask(key: &str) -> String {
    let n = key.chars().count();
    if n <= 8 {
        return "•".repeat(n);
    }
    let head: String = key.chars().take(3).collect();
    let tail: String = key.chars().skip(n - 4).collect();
    format!("{head}…{tail}")
}

/// Every provider we know how to talk to: registry entries + pool providers.
fn provider_rows(engine: &lz_core::Engine) -> Vec<Value> {
    let registry = engine.registry();
    let stored = engine.auth.all();
    let cat = lz_core::provider::pool::catalog();
    let models_dev = lz_core::provider::catalog::embedded();
    let mut rows: Vec<Value> = Vec::new();
    for p in registry.providers.values() {
        if p.id == lz_core::provider::pool::PROVIDER {
            continue;
        }
        let pool = cat.providers.get(&p.id);
        let env: Vec<String> = pool
            .map(|f| f.env.clone())
            .or_else(|| models_dev.get(&p.id).map(|c| c.env.clone()))
            .unwrap_or_default();
        let key = stored.get(&p.id).and_then(|a| match a {
            AuthInfo::Api { key, .. } => Some(key.clone()),
            _ => None,
        });
        rows.push(json!({
            "id": p.id, "name": p.name, "connected": p.connected(), "source": p.source,
            "models": p.models.len(), "env": env, "pool": pool.is_some(),
            "signup": pool.map(|f| f.signup.clone()), "note": pool.map(|f| f.note.clone()),
            "stored": key.is_some(), "masked": key.as_deref().map(mask),
        }));
    }
    rows.sort_by(|a, b| {
        let ka = (
            !a["connected"].as_bool().unwrap_or(false),
            !a["pool"].as_bool().unwrap_or(false),
            a["id"].as_str().unwrap_or("").to_string(),
        );
        let kb = (
            !b["connected"].as_bool().unwrap_or(false),
            !b["pool"].as_bool().unwrap_or(false),
            b["id"].as_str().unwrap_or("").to_string(),
        );
        ka.cmp(&kb)
    });
    rows
}

/// (provider, id, name, quality, speed, context, tools, limits)
type PoolEntry = (
    String,
    String,
    String,
    u32,
    u32,
    f64,
    bool,
    lz_schema::api::PoolInfo,
);

fn pool_rows(engine: &lz_core::Engine, all: bool) -> (Vec<Value>, u64, u64) {
    let registry = engine.registry();
    let usage = engine.router.usage(&registry);
    let cat = lz_core::provider::pool::catalog();
    let mut rows = Vec::new();
    let (mut req_today, mut tok_today) = (0u64, 0u64);
    for u in &usage {
        req_today += u.rpd_used;
        tok_today += u.tpd_used;
    }
    // catalog members, then anything the user added via `pool.include`
    let mut entries: Vec<PoolEntry> = cat
        .models
        .iter()
        .map(|m| {
            (
                m.provider.clone(),
                m.id.clone(),
                m.name.clone(),
                m.quality,
                m.speed,
                m.context,
                m.tools,
                cat.info(m),
            )
        })
        .collect();
    for p in registry.providers.values() {
        for m in p.models.values() {
            if let Some(info) = &m.pool
                && p.id != lz_core::provider::pool::PROVIDER
                && !cat.models.iter().any(|c| c.provider == p.id && c.id == m.id)
            {
                entries.push((
                    p.id.clone(),
                    m.id.clone(),
                    m.name.clone(),
                    info.quality,
                    info.speed,
                    m.limit.context,
                    m.tool_call,
                    info.clone(),
                ));
            }
        }
    }
    for (provider, id, name, quality, speed, context, tools, info) in entries {
        let connected = registry.providers.get(&provider).is_some_and(|p| p.connected());
        if !connected && !all {
            continue;
        }
        // connected but carrying no pool info → excluded by `pool.exclude`
        let excluded = connected && registry.get(&provider, &id).is_none_or(|m| m.pool.is_none());
        let u = usage.iter().find(|u| u.provider == provider && u.model == id);
        rows.push(json!({
            "provider": provider, "id": id, "name": name, "quality": quality, "speed": speed, "context": context, "tools": tools,
            "connected": connected, "excluded": excluded, "rpm": info.rpm, "rpd": info.rpd, "tpm": info.tpm, "tpd": info.tpd,
            "rpm_used": u.map(|u| u.rpm_used).unwrap_or(0), "rpd_used": u.map(|u| u.rpd_used).unwrap_or(0),
            "tpm_used": u.map(|u| u.tpm_used).unwrap_or(0), "tpd_used": u.map(|u| u.tpd_used).unwrap_or(0),
            "cooldown_secs": u.map(|u| u.cooldown_secs).unwrap_or(0), "last_error": u.map(|u| u.last_error.clone()).unwrap_or_default(),
            "ttft_ms": u.map(|u| u.ttft_ms).unwrap_or(0), "tps": u.map(|u| u.tps).unwrap_or(0),
        }));
    }
    (rows, req_today, tok_today)
}

async fn status(State(s): St) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let engine = &s.engine;
    let providers = provider_rows(engine);
    let (pool, req_today, tok_today) = pool_rows(engine, true);
    let ready = pool
        .iter()
        .filter(|m| {
            m["connected"].as_bool().unwrap_or(false) && m["cooldown_secs"].as_u64().unwrap_or(0) == 0
        })
        .count();
    let cooling = pool
        .iter()
        .filter(|m| m["cooldown_secs"].as_u64().unwrap_or(0) > 0)
        .count();
    let sessions = engine
        .list_sessions(SessionQuery {
            roots: true,
            limit: Some(500),
            ..Default::default()
        })
        .await
        .map_err(err)?;
    let day_ago = now_ms().saturating_sub(86_400_000);
    let config = engine.config();
    let default_model = engine.registry().default_model(&config).map(|m| m.full_id());
    Ok(Json(json!({
        "version": VERSION, "directory": engine.directory.display().to_string(),
        "providers": providers,
        "pool": { "ready": ready, "cooling": cooling, "total": pool.len(), "requests_today": req_today, "tokens_today": tok_today },
        "sessions": sessions.len(), "sessions_today": sessions.iter().filter(|s| s.time.updated >= day_ago).count(),
        "default_model": default_model,
    })))
}

async fn providers(State(s): St) -> Json<Value> {
    Json(
        json!({ "providers": provider_rows(&s.engine), "auth_path": s.engine.auth.path().display().to_string() }),
    )
}

#[derive(serde::Deserialize)]
struct KeyBody {
    key: String,
}

async fn auth_set(
    State(s): St,
    Path(provider): Path<String>,
    Json(body): Json<KeyBody>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    if body.key.trim().is_empty() {
        return Err(err("empty key"));
    }
    s.engine
        .set_auth(
            &provider,
            AuthInfo::Api {
                key: body.key.trim().to_string(),
                metadata: None,
            },
        )
        .await
        .map_err(err)?;
    Ok(Json(json!({ "ok": true })))
}

async fn auth_remove(
    State(s): St,
    Path(provider): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    s.engine.remove_auth(&provider).await.map_err(err)?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(serde::Deserialize)]
struct PoolQuery {
    #[serde(default)]
    all: Option<String>,
}

async fn pool(State(s): St, Query(q): Query<PoolQuery>) -> Json<Value> {
    let all = matches!(q.all.as_deref(), Some("1") | Some("true"));
    let (models, req_today, tok_today) = pool_rows(&s.engine, all);
    let cat = lz_core::provider::pool::catalog();
    let strategy = lz_core::provider::pool::config(&s.engine.config())
        .strategy
        .unwrap_or_else(|| "auto".into());
    Json(
        json!({ "models": models, "requests_today": req_today, "tokens_today": tok_today, "strategy": strategy, "catalog_version": cat.version, "total": cat.models.len() }),
    )
}

async fn pool_reset(State(s): St) -> Json<Value> {
    s.engine.router.reset_cooldowns();
    Json(json!({ "ok": true }))
}

async fn sessions(State(s): St) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let engine = &s.engine;
    let mut list = engine
        .list_sessions(SessionQuery {
            roots: true,
            limit: Some(40),
            ..Default::default()
        })
        .await
        .map_err(err)?;
    list.sort_by_key(|x| std::cmp::Reverse(x.time.updated));
    let mut out = Vec::new();
    for sess in list {
        let msgs = engine
            .messages(&sess.id, Default::default())
            .await
            .unwrap_or_default();
        let (mut tokens, mut cost) = (0.0, 0.0);
        for m in &msgs {
            if let lz_schema::session::Message::Assistant(a) = &m.info {
                cost += a.cost;
                let t = a.tokens.effective_total();
                if t > 0.0 {
                    tokens = t;
                }
            }
        }
        out.push(json!({
            "id": sess.id, "title": sess.title, "updated": sess.time.updated,
            "model": sess.model.as_ref().map(|m| format!("{}/{}", m.provider_id, m.id)),
            "tokens": tokens, "cost": cost,
        }));
    }
    Ok(Json(Value::Array(out)))
}

async fn config(State(s): St) -> Json<Value> {
    Json(
        json!({ "config": s.engine.config().as_ref().clone(), "global_dir": s.engine.paths.config.display().to_string() }),
    )
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
