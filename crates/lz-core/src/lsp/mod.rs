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

struct ClientState {
    rpc: Arc<JsonRpc>,
    #[allow(dead_code)]
    root: PathBuf,
    def: &'static ServerDef,
    /// path → (version, line count of the last text sent)
    versions: Mutex<HashMap<PathBuf, (i32, u64)>>,
    diagnostics: Arc<RwLock<HashMap<PathBuf, Vec<Diagnostic>>>>,
    updates: broadcast::Sender<PathBuf>,
    sync_kind: i64,
    /// Server supports `textDocument/diagnostic` pull requests.
    pull: bool,
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
        let diagnostics: Arc<RwLock<HashMap<PathBuf, Vec<Diagnostic>>>> =
            Arc::new(RwLock::new(HashMap::new()));
        let diag_store = diagnostics.clone();
        let rpc = JsonRpc::spawn(bin, args, &root, move |method, params| {
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
                let store = diag_store.clone();
                let tx = diag_tx.clone();
                tokio::spawn(async move {
                    store.write().await.insert(path.clone(), items);
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
                "workspace": { "workspaceFolders": true, "didChangeWatchedFiles": { "dynamicRegistration": false } }
            },
            "initializationOptions": def.initialization.clone().unwrap_or(json!({}))
        });
        let result = tokio::time::timeout(INITIALIZE_TIMEOUT, rpc.request("initialize", init))
            .await
            .map_err(|_| "initialize timed out".to_string())??;
        let sync_kind = match &result["capabilities"]["textDocumentSync"] {
            Value::Number(n) => n.as_i64().unwrap_or(1),
            Value::Object(o) => o.get("change").and_then(Value::as_i64).unwrap_or(1),
            _ => 1,
        };
        let pull = result["capabilities"]
            .get("diagnosticProvider")
            .is_some_and(|v| !v.is_null());
        rpc.notify("initialized", json!({})).await?;
        Ok(Arc::new(Self {
            pull,
            rpc,
            root,
            def,
            versions: Mutex::new(HashMap::new()),
            diagnostics,
            updates,
            sync_kind,
        }))
    }

    async fn touch(&self, path: &Path) -> Result<(), String> {
        let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        let mut versions = self.versions.lock().await;
        let language = self.def.language_id(path);
        let lines = text.matches('\n').count() as u64 + 1;
        match versions.get_mut(path) {
            None => {
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
                let changes = if self.sync_kind == 2 {
                    // incremental sync: replace the whole previous document range
                    json!([{ "range": { "start": { "line": 0, "character": 0 }, "end": { "line": *prev_lines + 1, "character": 0 } }, "text": text }])
                } else {
                    json!([{ "text": text }])
                };
                *prev_lines = lines;
                let version = *v;
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
        Ok(())
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
    async fn wait_for(&self, path: &Path) -> Vec<Diagnostic> {
        let mut rx = self.updates.subscribe();
        let wait = std::env::var("LZ_LSP_WAIT_MS")
            .ok()
            .and_then(|v| v.parse().ok())
            .map(Duration::from_millis)
            .unwrap_or(DIAGNOSTICS_WAIT);
        let deadline = tokio::time::Instant::now() + wait;
        if self.pull
            && let Ok(Some(items)) = tokio::time::timeout(wait, self.pull(path)).await
            && !items.is_empty()
        {
            return items;
        }
        let mut got = false;
        loop {
            let timeout = if got {
                DIAGNOSTICS_DEBOUNCE
            } else {
                deadline.saturating_duration_since(tokio::time::Instant::now())
            };
            match tokio::time::timeout(timeout, rx.recv()).await {
                Ok(Ok(p)) if p == path => got = true,
                Ok(Ok(_)) => {}
                Ok(Err(broadcast::error::RecvError::Lagged(_))) => got = true,
                _ => break,
            }
        }
        self.diagnostics
            .read()
            .await
            .get(path)
            .cloned()
            .unwrap_or_default()
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
        c.touch(path).await.ok()?;
        let diags = c.wait_for(path).await;
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
