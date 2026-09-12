//! Language-server client used for diagnostics after edits. Servers are
//! detected on PATH (no auto-install in v1). One client per (server, root).

pub mod jsonrpc;
pub mod servers;

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use lz_schema::api::LspStatus;
use serde_json::{Value, json};
use tokio::sync::{Mutex, RwLock, broadcast};

use jsonrpc::JsonRpc;
use servers::ServerDef;

const INITIALIZE_TIMEOUT: Duration = Duration::from_secs(45);
/// How long the first edit may wait for a server to finish loading the
/// workspace (rust-analyzer indexing, tsserver project load) before its
/// diagnostics are trusted; later edits find it ready.
const READY_WAIT: Duration = Duration::from_secs(90);
const DIAGNOSTICS_DEBOUNCE: Duration = Duration::from_millis(150);
const DIAGNOSTICS_WAIT: Duration = Duration::from_secs(5);
const MAX_PER_FILE: usize = 20;

#[derive(Debug, Clone)]
pub struct Diagnostic {
    pub severity: u8,
    pub line: u32,
    pub character: u32,
    pub message: String,
}

pub fn report(file: &Path, issues: &[Diagnostic]) -> Option<String> {
    let errors: Vec<&Diagnostic> = issues.iter().filter(|d| d.severity == 1).collect();
    if errors.is_empty() {
        return None;
    }
    let more = errors.len().saturating_sub(MAX_PER_FILE);
    let body = errors
        .iter()
        .take(MAX_PER_FILE)
        .map(|d| format!("ERROR [{}:{}] {}", d.line + 1, d.character + 1, d.message))
        .collect::<Vec<_>>()
        .join("\n");
    let suffix = if more > 0 {
        format!("\n... and {more} more")
    } else {
        String::new()
    };
    Some(format!(
        "<diagnostics file=\"{}\">\n{body}{suffix}\n</diagnostics>",
        file.display()
    ))
}

/// path → (document version the server reported for, diagnostics)
type DiagnosticStore = Arc<RwLock<HashMap<PathBuf, (Option<i64>, Vec<Diagnostic>)>>>;

struct ClientState {
    rpc: Arc<JsonRpc>,
    #[allow(dead_code)]
    root: PathBuf,
    def: &'static ServerDef,
    /// path → (version, line count of the last text sent)
    versions: Mutex<HashMap<PathBuf, (i32, u64)>>,
    /// path → (document version the server reported for, diagnostics)
    diagnostics: DiagnosticStore,
    updates: broadcast::Sender<PathBuf>,
    /// Server supports `textDocument/diagnostic` pull requests.
    pull: bool,
    /// `true` once every `$/progress` cycle the server started has ended
    /// (or it never reported any): an empty diagnostics set means clean.
    ready: tokio::sync::watch::Receiver<bool>,
}

fn uri(path: &Path) -> String {
    format!("file://{}", path.display())
}

fn path_from_uri(u: &str) -> Option<PathBuf> {
    let p = u.strip_prefix("file://")?;
    let decoded: String = percent_decode(p);
    Some(PathBuf::from(decoded))
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16)
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

impl ClientState {
    async fn start(def: &'static ServerDef, root: PathBuf) -> Result<Arc<Self>, String> {
        let (bin, args) = def.command.split_first().ok_or("empty command")?;
        let (updates, _) = broadcast::channel(64);
        let diag_tx = updates.clone();
        let diagnostics: DiagnosticStore = Arc::new(RwLock::new(HashMap::new()));
        let diag_store = diagnostics.clone();
        let (ready_tx, ready_rx) = tokio::sync::watch::channel(false);
        let progress = Arc::new(std::sync::Mutex::new((0usize, false))); // (active, seen any)
        let progress_for_cb = progress.clone();
        let ready_for_cb = ready_tx.clone();
        let rpc = JsonRpc::spawn(bin, args, &root, move |method, params| {
            tracing::debug!(
                method,
                "lsp notification: {}",
                params.to_string().chars().take(200).collect::<String>()
            );
            if method == "$/progress" {
                let kind = params["value"]["kind"].as_str().unwrap_or("");
                let mut p = progress_for_cb.lock().unwrap_or_else(|e| e.into_inner());
                match kind {
                    "begin" => {
                        p.0 += 1;
                        p.1 = true;
                        let _ = ready_for_cb.send(false);
                    }
                    "end" => {
                        p.0 = p.0.saturating_sub(1);
                        if p.0 == 0 {
                            let _ = ready_for_cb.send(true);
                        }
                    }
                    _ => {}
                }
                return;
            }
            if method == "textDocument/publishDiagnostics" {
                let Some(path) = params.get("uri").and_then(Value::as_str).and_then(path_from_uri) else {
                    return;
                };
                let items: Vec<Diagnostic> = params["diagnostics"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .map(|d| Diagnostic {
                                severity: d["severity"].as_u64().unwrap_or(1) as u8,
                                line: d["range"]["start"]["line"].as_u64().unwrap_or(0) as u32,
                                character: d["range"]["start"]["character"].as_u64().unwrap_or(0) as u32,
                                message: d["message"].as_str().unwrap_or("").to_string(),
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                let version = params["version"].as_i64();
                let store = diag_store.clone();
                let tx = diag_tx.clone();
                tokio::spawn(async move {
                    store.write().await.insert(path.clone(), (version, items));
                    let _ = tx.send(path);
                });
            }
        })
        .await?;
        let init = json!({
            "processId": std::process::id(),
            "rootUri": uri(&root),
            "rootPath": root.display().to_string(),
            "workspaceFolders": [{ "uri": uri(&root), "name": root.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default() }],
            "capabilities": {
                "textDocument": {
                    "synchronization": { "dynamicRegistration": false, "didSave": true },
                    "publishDiagnostics": { "relatedInformation": true, "versionSupport": true }
                },
                "workspace": { "workspaceFolders": true, "didChangeWatchedFiles": { "dynamicRegistration": false } },
                // without this, servers never report load/index progress and an
                // empty diagnostics set would be indistinguishable from "still loading"
                "window": { "workDoneProgress": true }
            },
            "initializationOptions": def.initialization.clone().unwrap_or(json!({}))
        });
        let result = tokio::time::timeout(INITIALIZE_TIMEOUT, rpc.request("initialize", init))
            .await
            .map_err(|_| "initialize timed out".to_string())??;
        let pull = result["capabilities"]
            .get("diagnosticProvider")
            .is_some_and(|v| !v.is_null());
        rpc.notify("initialized", json!({})).await?;
        // servers that never report progress are ready right away; give the
        // others a moment to announce their first load
        let progress_grace = progress.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(1500)).await;
            let p = progress_grace.lock().unwrap_or_else(|e| e.into_inner());
            if !p.1 {
                let _ = ready_tx.send(true);
            }
        });
        Ok(Arc::new(Self {
            pull,
            rpc,
            root,
            def,
            versions: Mutex::new(HashMap::new()),
            diagnostics,
            updates,
            ready: ready_rx,
        }))
    }

    /// Wait until the server has finished loading: no `$/progress` cycle
    /// active for a quiet period (rust-analyzer chains several — fetching,
    /// crate graph, proc-macros, cache priming — with sub-millisecond gaps).
    async fn wait_ready(&self, max: Duration) -> bool {
        const QUIET: Duration = Duration::from_millis(600);
        let mut rx = self.ready.clone();
        let deadline = tokio::time::Instant::now() + max;
        loop {
            // wait for ready = true
            while !*rx.borrow() {
                let left = deadline.saturating_duration_since(tokio::time::Instant::now());
                if left.is_zero() {
                    return false;
                }
                if tokio::time::timeout(left, rx.changed()).await.is_err() {
                    return false;
                }
            }
            // …and for it to stay true
            match tokio::time::timeout(QUIET, rx.changed()).await {
                Err(_) => return *rx.borrow(),
                Ok(Ok(())) => continue,
                Ok(Err(_)) => return *rx.borrow(),
            }
        }
    }

    async fn touch(&self, path: &Path) -> Result<i32, String> {
        let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        let mut versions = self.versions.lock().await;
        let language = self.def.language_id(path);
        let lines = text.matches('\n').count() as u64 + 1;
        let sent_version;
        match versions.get_mut(path) {
            None => {
                sent_version = 1;
                versions.insert(path.to_path_buf(), (1, lines));
                self.rpc
                    .notify(
                        "textDocument/didOpen",
                        json!({ "textDocument": { "uri": uri(path), "languageId": language, "version": 1, "text": text } }),
                    )
                    .await?;
            }
            Some((v, prev_lines)) => {
                *v += 1;
                // a change without a range replaces the whole document — valid
                // for full and incremental sync alike (a synthetic range past the
                // old end is rejected by rust-analyzer, which then keeps the old text)
                let changes = json!([{ "text": text }]);
                *prev_lines = lines;
                let version = *v;
                sent_version = version;
                self.rpc
                    .notify(
                        "textDocument/didChange",
                        json!({ "textDocument": { "uri": uri(path), "version": version }, "contentChanges": changes }),
                    )
                    .await?;
            }
        }
        self.rpc
            .notify(
                "workspace/didChangeWatchedFiles",
                json!({ "changes": [{ "uri": uri(path), "type": 2 }] }),
            )
            .await?;
        Ok(sent_version)
    }

    /// Pull diagnostics for the document (servers with `diagnosticProvider`).
    async fn pull(&self, path: &Path) -> Option<Vec<Diagnostic>> {
        if !self.pull {
            return None;
        }
        let r = self
            .rpc
            .request(
                "textDocument/diagnostic",
                json!({ "textDocument": { "uri": uri(path) } }),
            )
            .await
            .ok()?;
        let items = r.get("items")?.as_array()?;
        Some(
            items
                .iter()
                .map(|d| Diagnostic {
                    severity: d["severity"].as_u64().unwrap_or(1) as u8,
                    line: d["range"]["start"]["line"].as_u64().unwrap_or(0) as u32,
                    character: d["range"]["start"]["character"].as_u64().unwrap_or(0) as u32,
                    message: d["message"].as_str().unwrap_or("").to_string(),
                })
                .collect(),
        )
    }

    /// Wait (debounced) for a diagnostics push for `path`, racing a pull.
    /// `rx` must have been subscribed before the change was sent; pushes that
    /// arrived while the server was still loading are placeholders and are
    /// skipped.
    async fn wait_for(
        &self,
        path: &Path,
        mut rx: broadcast::Receiver<PathBuf>,
        loaded_now: bool,
        version: i32,
    ) -> Vec<Diagnostic> {
        let wait = std::env::var("LZ_LSP_WAIT_MS")
            .ok()
            .and_then(|v| v.parse().ok())
            .map(Duration::from_millis)
            .unwrap_or(DIAGNOSTICS_WAIT);
        if loaded_now {
            while rx.try_recv().is_ok() {}
        }
        let deadline = tokio::time::Instant::now() + wait;
        if self.pull
            && let Ok(Some(items)) = tokio::time::timeout(wait, self.pull(path)).await
            && !items.is_empty()
        {
            return items;
        }
        // a push is current when the server tags it with our version (or
        // doesn't version at all); older ones describe the previous text
        let current = |v: Option<i64>| v.is_none_or(|v| v >= version as i64);
        let mut got = false;
        loop {
            let timeout = if got {
                DIAGNOSTICS_DEBOUNCE
            } else {
                deadline.saturating_duration_since(tokio::time::Instant::now())
            };
            match tokio::time::timeout(timeout, rx.recv()).await {
                Ok(Ok(p)) if p == path => {
                    let v = self.diagnostics.read().await.get(path).and_then(|d| d.0);
                    if current(v) {
                        got = true;
                    }
                }
                Ok(Ok(_)) => {}
                Ok(Err(broadcast::error::RecvError::Lagged(_))) => got = true,
                _ => break,
            }
        }
        let store = self.diagnostics.read().await;
        match store.get(path) {
            Some((v, items)) if current(*v) => items.clone(),
            _ => Vec::new(),
        }
    }
}

#[derive(Default)]
pub struct LspManager {
    clients: RwLock<BTreeMap<(String, PathBuf), Arc<ClientState>>>,
    failed: RwLock<BTreeMap<(String, PathBuf), String>>,
    pub enabled: bool,
}

impl LspManager {
    pub fn new(enabled: bool) -> Self {
        Self {
            clients: RwLock::new(BTreeMap::new()),
            failed: RwLock::new(BTreeMap::new()),
            enabled,
        }
    }

    async fn client_for(&self, path: &Path, worktree: &Path) -> Option<Arc<ClientState>> {
        if !self.enabled {
            return None;
        }
        let def = servers::for_path(path)?;
        let root = def.find_root(path, worktree)?;
        let key = (def.id.to_string(), root.clone());
        if let Some(c) = self.clients.read().await.get(&key) {
            return Some(c.clone());
        }
        if self.failed.read().await.contains_key(&key) {
            return None;
        }
        if !servers::on_path(def.command[0]) {
            self.failed
                .write()
                .await
                .insert(key, format!("{} not found on PATH", def.command[0]));
            return None;
        }
        match ClientState::start(def, root.clone()).await {
            Ok(c) => {
                tracing::info!(server = def.id, root = %root.display(), "lsp started");
                self.clients.write().await.insert(key, c.clone());
                Some(c)
            }
            Err(e) => {
                tracing::warn!(server = def.id, "lsp failed: {e}");
                self.failed.write().await.insert(key, e);
                None
            }
        }
    }

    /// Open/update the file; used by `read` to warm servers up.
    pub async fn touch(&self, path: &Path, worktree: &Path) {
        if let Some(c) = self.client_for(path, worktree).await {
            let _ = c.touch(path).await;
        }
    }

    /// Open/update the file and wait for fresh diagnostics.
    pub async fn diagnostics_after_edit(&self, path: &Path, worktree: &Path) -> Option<String> {
        let c = self.client_for(path, worktree).await?;
        let rx = c.updates.subscribe();
        let was_ready = *c.ready.borrow();
        let version = c.touch(path).await.ok()?;
        let t = std::time::Instant::now();
        if !c.wait_ready(READY_WAIT).await {
            tracing::debug!(
                server = c.def.id,
                "still loading; skipping diagnostics for this edit"
            );
            return None;
        }
        if !was_ready {
            tracing::info!(server = c.def.id, "language server ready in {:?}", t.elapsed());
        }
        let diags = c.wait_for(path, rx, !was_ready, version).await;
        report(path, &diags)
    }

    pub async fn status(&self) -> Vec<LspStatus> {
        let mut out: Vec<LspStatus> = self
            .clients
            .read()
            .await
            .iter()
            .map(|((id, root), c)| LspStatus {
                id: id.clone(),
                name: c.def.name.into(),
                root: root.display().to_string(),
                status: "connected".into(),
            })
            .collect();
        for ((id, root), err) in self.failed.read().await.iter() {
            out.push(LspStatus {
                id: id.clone(),
                name: id.clone(),
                root: root.display().to_string(),
                status: format!("error: {err}"),
            });
        }
        out
    }

    pub async fn shutdown(&self) {
        let clients: Vec<Arc<ClientState>> = std::mem::take(&mut *self.clients.write().await)
            .into_values()
            .collect();
        for c in clients {
            let _ =
                tokio::time::timeout(Duration::from_secs(2), c.rpc.request("shutdown", Value::Null)).await;
            let _ = c.rpc.notify("exit", Value::Null).await;
        }
    }
}
