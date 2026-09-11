//! LunarZero's local web portal: a token-protected HTTP server on 127.0.0.1
//! that shares the running engine with the TUI. Chat (with live events over
//! SSE), permissions/questions, API keys, the free pool, sessions and a
//! settings editor. Single embedded page, no build step.

use std::convert::Infallible;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{Html, IntoResponse, Json};
use axum::routing::{get, post, put};
use axum::{Router, middleware};
use futures::StreamExt;
use lz_schema::api::*;
use lz_schema::session::*;
use serde_json::{Value, json};

const INDEX: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../assets/web/index.html"
));

struct AppState {
    engine: Arc<lz_core::Engine>,
    token: String,
    version: String,
}

type St = State<Arc<AppState>>;
type ApiErr = (StatusCode, Json<Value>);

/// A running portal.
pub struct Portal {
    pub url: String,
    pub port: u16,
    task: tokio::task::JoinHandle<()>,
}

impl Portal {
    pub fn stop(&self) {
        self.task.abort();
    }
}

impl Drop for Portal {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn new_token() -> String {
    use rand::RngCore;
    let mut b = [0u8; 16];
    rand::rng().fill_bytes(&mut b);
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Bind on 127.0.0.1:`port` (0 = any free port) and serve in the background.
pub async fn start(engine: Arc<lz_core::Engine>, port: u16, version: &str) -> anyhow::Result<Portal> {
    let token = new_token();
    let state = Arc::new(AppState {
        engine,
        token: token.clone(),
        version: version.to_string(),
    });
    let api = Router::new()
        .route("/status", get(status))
        .route("/providers", get(providers))
        .route("/auth/{provider}", put(auth_set).delete(auth_remove))
        .route("/pool", get(pool))
        .route("/pool/reset", post(pool_reset))
        .route("/sessions", get(sessions))
        .route("/session", post(session_create))
        .route("/session/{id}", axum::routing::delete(session_delete))
        .route("/session/{id}/messages", get(session_messages))
        .route("/session/{id}/prompt", post(session_prompt))
        .route("/session/{id}/abort", post(session_abort))
        .route("/permission/{id}", post(permission_reply))
        .route("/question/{id}", post(question_reply))
        .route("/agents", get(agents))
        .route("/skills", get(skills_list).post(skill_install))
        .route("/skills/{name}", axum::routing::delete(skill_remove))
        .route("/models", get(models))
        .route("/config", get(config_get))
        .route("/config/{scope}", put(config_put))
        .route("/events", get(events))
        .layer(middleware::from_fn_with_state(state.clone(), require_token));
    let app = Router::new()
        .route("/", get(|| async { Html(INDEX) }))
        .nest("/api", api)
        .with_state(state);
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
    let addr = listener.local_addr()?;
    let url = format!("http://{addr}/?token={token}");
    let task = tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            tracing::error!("web portal stopped: {e}");
        }
    });
    Ok(Portal {
        url,
        port: addr.port(),
        task,
    })
}

/// Serve until ctrl-c (the `lz web` command).
pub async fn serve_blocking(
    engine: Arc<lz_core::Engine>,
    port: u16,
    version: &str,
    open: bool,
) -> anyhow::Result<()> {
    let portal = start(engine, port, version).await?;
    println!("LunarZero portal: {}\n(ctrl+c to stop)", portal.url);
    if open {
        open_browser(&portal.url);
    }
    tokio::signal::ctrl_c().await?;
    portal.stop();
    Ok(())
}

pub fn open_browser(url: &str) {
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let _ = std::process::Command::new(opener)
        .arg(url)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

#[derive(serde::Deserialize, Default)]
struct TokenQuery {
    token: Option<String>,
}

/// Two ways in: the per-run token (scripts, curl, the `?token=` link), or a
/// browser request that is genuinely same-origin to this loopback server.
/// Browsers set `Host`, `Origin` and `Sec-Fetch-Site` themselves, so a page
/// from another site cannot make its requests look same-origin to 127.0.0.1.
async fn require_token(
    State(state): St,
    Query(q): Query<TokenQuery>,
    headers: HeaderMap,
    req: axum::extract::Request,
    next: middleware::Next,
) -> axum::response::Response {
    let hdr = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
    };
    let from_header = hdr("authorization").and_then(|v| v.strip_prefix("Bearer ").map(str::to_string));
    let token_ok = from_header.or(q.token).is_some_and(|t| t == state.token);
    let same_origin = || {
        let host = hdr("host").unwrap_or_default();
        let host_ok =
            host.starts_with("127.0.0.1:") || host.starts_with("localhost:") || host.starts_with("[::1]:");
        let site_ok = matches!(
            hdr("sec-fetch-site").as_deref(),
            Some("same-origin") | Some("none")
        );
        let origin_ok = match hdr("origin") {
            None => true,
            Some(o) => o.strip_prefix("http://").is_some_and(|h| h == host),
        };
        host_ok && site_ok && origin_ok
    };
    if !token_ok && !same_origin() {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "message": "unauthorized: open the portal from the link in the terminal footer" })),
        )
            .into_response();
    }
    next.run(req).await
}

fn err(e: impl std::fmt::Display) -> ApiErr {
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

// ───────────────────────────── providers / keys ─────────────────────────────

fn provider_rows(engine: &lz_core::Engine) -> Vec<Value> {
    let registry = engine.registry();
    let stored = engine.auth.all();
    let cat = lz_core::provider::pool::catalog();
    let known = lz_core::provider::catalog::embedded();
    let mut rows: Vec<Value> = Vec::new();
    for p in registry.providers.values() {
        if p.id == lz_core::provider::pool::PROVIDER {
            continue;
        }
        let pool = cat.providers.get(&p.id);
        let env: Vec<String> = pool
            .map(|f| f.env.clone())
            .or_else(|| known.get(&p.id).map(|c| c.env.clone()))
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
    rows.sort_by_key(|a| {
        (
            !a["connected"].as_bool().unwrap_or(false),
            !a["pool"].as_bool().unwrap_or(false),
            a["id"].as_str().unwrap_or("").to_string(),
        )
    });
    rows
}

/// (provider, id, name, quality, speed, context, tools, limits)
type PoolEntry = (String, String, String, u32, u32, f64, bool, PoolInfo);

fn pool_rows(engine: &lz_core::Engine, all: bool) -> (Vec<Value>, u64, u64) {
    let registry = engine.registry();
    let usage = engine.router.usage(&registry);
    let cat = lz_core::provider::pool::catalog();
    let (req_today, tok_today) = usage
        .iter()
        .fold((0u64, 0u64), |(r, t), u| (r + u.rpd_used, t + u.tpd_used));
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
    let mut rows = Vec::new();
    for (provider, id, name, quality, speed, context, tools, info) in entries {
        let connected = registry.providers.get(&provider).is_some_and(|p| p.connected());
        if !connected && !all {
            continue;
        }
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

async fn status(State(s): St) -> Result<Json<Value>, ApiErr> {
    let engine = &s.engine;
    let providers = provider_rows(engine);
    let (pool, req_today, tok_today) = pool_rows(engine, true);
    let ready = pool
        .iter()
        .filter(|m| {
            m["connected"].as_bool().unwrap_or(false)
                && !m["excluded"].as_bool().unwrap_or(false)
                && m["cooldown_secs"].as_u64().unwrap_or(0) == 0
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
        "version": s.version, "directory": engine.directory.display().to_string(),
        "providers": providers,
        "pool": { "ready": ready, "cooling": cooling, "total": pool.len(), "requests_today": req_today, "tokens_today": tok_today },
        "sessions": sessions.len(), "sessions_today": sessions.iter().filter(|s| s.time.updated >= day_ago).count(),
        "default_model": default_model, "default_agent": config.default_agent,
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
) -> Result<Json<Value>, ApiErr> {
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

async fn auth_remove(State(s): St, Path(provider): Path<String>) -> Result<Json<Value>, ApiErr> {
    s.engine.remove_auth(&provider).await.map_err(err)?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(serde::Deserialize)]
struct PoolQuery {
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

// ───────────────────────────── sessions / chat ─────────────────────────────

async fn sessions(State(s): St) -> Result<Json<Value>, ApiErr> {
    let engine = &s.engine;
    let mut list = engine
        .list_sessions(SessionQuery {
            roots: true,
            limit: Some(60),
            ..Default::default()
        })
        .await
        .map_err(err)?;
    list.sort_by_key(|x| std::cmp::Reverse(x.time.updated));
    let status = engine.status.all();
    let out: Vec<Value> = list
        .into_iter()
        .map(|sess| {
            json!({
                "id": sess.id, "title": sess.title, "updated": sess.time.updated, "directory": sess.directory,
                "model": sess.model.as_ref().map(|m| format!("{}/{}", m.provider_id, m.id)), "agent": sess.agent,
                "busy": status.get(&sess.id).is_some_and(|st| !matches!(st, SessionStatus::Idle)),
            })
        })
        .collect();
    Ok(Json(Value::Array(out)))
}

#[derive(serde::Deserialize, Default)]
struct CreateBody {
    title: Option<String>,
    agent: Option<String>,
}

async fn session_create(State(s): St, body: Option<Json<CreateBody>>) -> Result<Json<Value>, ApiErr> {
    let b = body.map(|b| b.0).unwrap_or_default();
    let info = s
        .engine
        .create_session(CreateSession {
            title: b.title,
            agent: b.agent,
            ..Default::default()
        })
        .await
        .map_err(err)?;
    Ok(Json(serde_json::to_value(info).unwrap_or(Value::Null)))
}

async fn session_delete(State(s): St, Path(id): Path<String>) -> Result<Json<Value>, ApiErr> {
    s.engine.delete_session(&id).await.map_err(err)?;
    Ok(Json(json!({ "ok": true })))
}

async fn session_messages(State(s): St, Path(id): Path<String>) -> Result<Json<Value>, ApiErr> {
    let msgs = s
        .engine
        .messages(&id, MessagesQuery::default())
        .await
        .map_err(err)?;
    let info = s.engine.get_session(&id).await.map_err(err)?;
    let (mut tokens, mut cost) = (0.0, 0.0);
    for m in &msgs {
        if let Message::Assistant(a) = &m.info {
            cost += a.cost;
            let t = a.tokens.effective_total();
            if t > 0.0 {
                tokens = t;
            }
        }
    }
    let permissions: Vec<PermissionRequest> = s
        .engine
        .pending_permissions()
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|p| p.session_id == id)
        .collect();
    let questions: Vec<QuestionRequest> = s
        .engine
        .pending_questions()
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|q| q.session_id == id)
        .collect();
    let busy = !matches!(s.engine.status.get(&id), SessionStatus::Idle);
    Ok(Json(
        json!({ "session": info, "messages": msgs, "tokens": tokens, "cost": cost, "busy": busy, "permissions": permissions, "questions": questions }),
    ))
}

#[derive(serde::Deserialize)]
struct PromptBody {
    text: String,
    model: Option<String>,
    agent: Option<String>,
    variant: Option<String>,
}

async fn session_prompt(
    State(s): St,
    Path(id): Path<String>,
    Json(b): Json<PromptBody>,
) -> Result<Json<Value>, ApiErr> {
    let text = b.text.trim().to_string();
    if text.is_empty() {
        return Err(err("empty prompt"));
    }
    let model = match b.model.as_deref().filter(|m| !m.is_empty()) {
        Some(spec) => {
            let (p, m) = spec
                .split_once('/')
                .ok_or_else(|| err("model must be provider/model"))?;
            Some(ModelRef {
                provider_id: p.into(),
                model_id: m.into(),
                variant: b.variant.clone(),
            })
        }
        None => None,
    };
    // `/command args` and `!shell` work here too
    if let Some(cmd) = text.strip_prefix('!') {
        s.engine
            .shell(
                &id,
                ShellRequest {
                    command: cmd.trim().into(),
                    agent: b.agent.clone(),
                    model: model.clone(),
                },
            )
            .await
            .map_err(err)?;
        return Ok(Json(json!({ "ok": true })));
    }
    if let Some(rest) = text.strip_prefix('/')
        && !rest.starts_with('/')
    {
        let (name, args) = rest
            .split_once(char::is_whitespace)
            .map(|(n, a)| (n.to_string(), a.trim().to_string()))
            .unwrap_or((rest.trim().to_string(), String::new()));
        let known = EngineApi::commands(&*s.engine)
            .await
            .unwrap_or_default()
            .into_iter()
            .any(|c| c.name == name);
        if known {
            s.engine
                .command(
                    &id,
                    CommandRequest {
                        command: name,
                        arguments: args,
                        agent: b.agent.clone(),
                        model: model.clone(),
                    },
                )
                .await
                .map_err(err)?;
            return Ok(Json(json!({ "ok": true })));
        }
    }
    let req = PromptRequest {
        model,
        agent: b.agent.filter(|a| !a.is_empty()),
        variant: b.variant,
        parts: vec![PartInput::Text {
            id: None,
            text,
            synthetic: false,
            ignored: false,
        }],
        ..Default::default()
    };
    s.engine.prompt_async(&id, req).await.map_err(err)?;
    Ok(Json(json!({ "ok": true })))
}

async fn session_abort(State(s): St, Path(id): Path<String>) -> Result<Json<Value>, ApiErr> {
    s.engine.abort(&id).await.map_err(err)?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(serde::Deserialize)]
struct PermissionBody {
    reply: String,
    message: Option<String>,
}

async fn permission_reply(
    State(s): St,
    Path(id): Path<String>,
    Json(b): Json<PermissionBody>,
) -> Result<Json<Value>, ApiErr> {
    let reply = match b.reply.as_str() {
        "once" => PermissionReply::Once,
        "always" => PermissionReply::Always,
        _ => PermissionReply::Reject,
    };
    s.engine
        .reply_permission(
            &id,
            PermissionReplyRequest {
                reply,
                message: b.message.filter(|m| !m.trim().is_empty()),
            },
        )
        .await
        .map_err(err)?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(serde::Deserialize)]
struct QuestionBody {
    answers: Option<Vec<Vec<String>>>,
    reject: Option<bool>,
}

async fn question_reply(
    State(s): St,
    Path(id): Path<String>,
    Json(b): Json<QuestionBody>,
) -> Result<Json<Value>, ApiErr> {
    if b.reject.unwrap_or(false) {
        s.engine.reject_question(&id).await.map_err(err)?;
    } else {
        s.engine
            .reply_question(&id, b.answers.unwrap_or_default())
            .await
            .map_err(err)?;
    }
    Ok(Json(json!({ "ok": true })))
}

async fn agents(State(s): St) -> Result<Json<Value>, ApiErr> {
    let list = EngineApi::agents(&*s.engine).await.map_err(err)?;
    Ok(Json(serde_json::to_value(list).unwrap_or(Value::Null)))
}

async fn skills_list(State(s): St) -> Result<Json<Value>, ApiErr> {
    let list = EngineApi::skills(&*s.engine).await.map_err(err)?;
    let rows: Vec<Value> = list
        .into_iter()
        .map(|sk| {
            let installed = std::path::Path::new(&sk.location).parent().is_some_and(|d| d.join(lz_core::skill_install::META_FILE).exists());
            json!({ "name": sk.name, "description": sk.description, "location": sk.location, "installed": installed })
        })
        .collect();
    Ok(Json(Value::Array(rows)))
}

#[derive(serde::Deserialize)]
struct SkillInstallBody {
    source: String,
    #[serde(default)]
    project: bool,
}

async fn skill_install(State(s): St, Json(b): Json<SkillInstallBody>) -> Result<Json<Value>, ApiErr> {
    let list = s.engine.install_skill(&b.source, !b.project).await.map_err(err)?;
    Ok(Json(serde_json::to_value(list).unwrap_or(Value::Null)))
}

async fn skill_remove(State(s): St, Path(name): Path<String>) -> Result<Json<Value>, ApiErr> {
    s.engine.remove_skill(&name).await.map_err(err)?;
    Ok(Json(json!({ "ok": true })))
}

async fn models(State(s): St) -> Result<Json<Value>, ApiErr> {
    let p = s.engine.providers().await.map_err(err)?;
    Ok(Json(serde_json::to_value(p).unwrap_or(Value::Null)))
}

/// Live engine events as SSE (`{type, properties}` JSON per event).
async fn events(State(s): St) -> Sse<impl futures::Stream<Item = Result<SseEvent, Infallible>>> {
    let stream = s.engine.subscribe().map(|e| {
        let data = serde_json::to_string(&e).unwrap_or_else(|_| "{}".into());
        Ok(SseEvent::default().event(e.type_name()).data(data))
    });
    Sse::new(stream).keep_alive(KeepAlive::default())
}

// ───────────────────────────── settings ─────────────────────────────

fn config_files(engine: &lz_core::Engine) -> Vec<(String, std::path::PathBuf, &'static str)> {
    let global = {
        let dir = &engine.paths.config;
        ["lunarzero.jsonc", "lunarzero.json", "config.json"]
            .iter()
            .map(|n| dir.join(n))
            .find(|p| p.exists())
            .unwrap_or_else(|| dir.join("lunarzero.json"))
    };
    let project = {
        let dir = &engine.directory;
        [
            "lunarzero.jsonc",
            "lunarzero.json",
            "opencode.jsonc",
            "opencode.json",
        ]
        .iter()
        .map(|n| dir.join(n))
        .find(|p| p.exists())
        .unwrap_or_else(|| dir.join("lunarzero.json"))
    };
    let tui = engine.paths.config.join("tui.json");
    vec![
        ("global".into(), global, "Applies to every project"),
        ("project".into(), project, "This project only; overrides global"),
        ("tui".into(), tui, "Terminal UI: theme, keybinds, web portal"),
    ]
}

async fn config_get(State(s): St) -> Json<Value> {
    let engine = &s.engine;
    let files: Vec<Value> = config_files(engine)
        .into_iter()
        .map(|(scope, path, desc)| {
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            json!({ "scope": scope, "path": path.display().to_string(), "exists": path.exists(), "text": text, "description": desc })
        })
        .collect();
    let agents: Vec<Value> = EngineApi::agents(&**engine).await.unwrap_or_default().into_iter().filter(|a| !a.hidden).map(|a| json!({ "name": a.name, "description": a.description, "mode": a.mode, "builtin": a.builtin, "model": a.model.map(|m| format!("{}/{}", m.provider_id, m.model_id)) })).collect();
    let themes = lz_core::config::load_tui(&engine.paths, &engine.config_dirs()).theme;
    Json(
        json!({ "merged": engine.config().as_ref().clone(), "files": files, "agents": agents, "theme": themes, "schema_url": "lz config schema" }),
    )
}

#[derive(serde::Deserialize)]
struct ConfigBody {
    text: String,
}

async fn config_put(
    State(s): St,
    Path(scope): Path<String>,
    Json(b): Json<ConfigBody>,
) -> Result<Json<Value>, ApiErr> {
    let engine = &s.engine;
    let (_, path, _) = config_files(engine)
        .into_iter()
        .find(|(sc, _, _)| *sc == scope)
        .ok_or_else(|| err("unknown scope"))?;
    let text = if b.text.trim().is_empty() {
        "{}\n".to_string()
    } else {
        b.text.clone()
    };
    lz_core::config::parse_jsonc(&text, &path).map_err(|e| err(format!("invalid JSON: {e}")))?;
    if scope != "tui" {
        let v = lz_core::config::parse_jsonc(&text, &path).map_err(err)?;
        serde_json::from_value::<lz_schema::config::Config>(v)
            .map_err(|e| err(format!("invalid config: {e}")))?;
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(err)?;
    }
    std::fs::write(&path, &text).map_err(err)?;
    if scope != "tui" {
        engine.reload().await.map_err(err)?;
    }
    Ok(Json(json!({ "ok": true, "path": path.display().to_string() })))
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
