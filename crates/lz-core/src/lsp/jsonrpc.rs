//! Minimal JSON-RPC 2.0 over stdio with `Content-Length` framing (LSP wire).

use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{Mutex, oneshot};

type Pending = Arc<Mutex<HashMap<i64, oneshot::Sender<Result<Value, String>>>>>;

pub struct JsonRpc {
    stdin: Mutex<tokio::process::ChildStdin>,
    pending: Pending,
    next_id: AtomicI64,
    _child: Mutex<tokio::process::Child>,
}

impl JsonRpc {
    pub async fn spawn(
        bin: &str,
        args: &[&str],
        cwd: &Path,
        on_notification: impl Fn(&str, Value) + Send + Sync + 'static,
    ) -> Result<Arc<Self>, String> {
        let mut child = tokio::process::Command::new(bin)
            .args(args)
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("failed to start {bin}: {e}"))?;
        let stdin = child.stdin.take().ok_or("no stdin")?;
        let stdout = child.stdout.take().ok_or("no stdout")?;
        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let rpc = Arc::new(Self {
            stdin: Mutex::new(stdin),
            pending: pending.clone(),
            next_id: AtomicI64::new(1),
            _child: Mutex::new(child),
        });
        let reader_rpc = rpc.clone();
        let pending_for_eof = pending.clone();
        tokio::spawn(async move {
            let mut reader = BufReader::new(stdout);
            read_loop(&mut reader, &pending, &reader_rpc, &on_notification).await;
            // server went away: fail every outstanding request
            for (_, tx) in pending_for_eof.lock().await.drain() {
                let _ = tx.send(Err("language server exited".into()));
            }
        });
        Ok(rpc)
    }
}

async fn read_loop(
    reader: &mut BufReader<tokio::process::ChildStdout>,
    pending: &Pending,
    reader_rpc: &Arc<JsonRpc>,
    on_notification: &(impl Fn(&str, Value) + Send + Sync + 'static),
) {
    loop {
        // headers
        let mut length: Option<usize> = None;
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line).await {
                Ok(0) | Err(_) => return,
                Ok(_) => {}
            }
            let line = line.trim_end_matches(['\r', '\n']);
            if line.is_empty() {
                break;
            }
            if let Some(v) = line.strip_prefix("Content-Length:") {
                length = v.trim().parse().ok();
            }
        }
        let Some(len) = length else { continue };
        let mut buf = vec![0u8; len];
        if reader.read_exact(&mut buf).await.is_err() {
            return;
        }
        let Ok(msg) = serde_json::from_slice::<Value>(&buf) else {
            continue;
        };
        if let Some(id) = msg
            .get("id")
            .and_then(Value::as_i64)
            .filter(|_| msg.get("method").is_none())
        {
            if let Some(tx) = pending.lock().await.remove(&id) {
                let result = if let Some(err) = msg.get("error") {
                    Err(err["message"].as_str().unwrap_or("rpc error").to_string())
                } else {
                    Ok(msg.get("result").cloned().unwrap_or(Value::Null))
                };
                let _ = tx.send(result);
            }
        } else if let Some(method) = msg.get("method").and_then(Value::as_str) {
            let params = msg.get("params").cloned().unwrap_or(Value::Null);
            if let Some(id) = msg.get("id") {
                // server → client request: answer the common ones, null the rest
                let result = match method {
                    "workspace/configuration" => json!([null]),
                    "client/registerCapability"
                    | "client/unregisterCapability"
                    | "window/workDoneProgress/create" => Value::Null,
                    _ => Value::Null,
                };
                let _ = reader_rpc
                    .send(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
                    .await;
            } else {
                on_notification(method, params);
            }
        }
    }
}

impl JsonRpc {
    async fn send(&self, msg: Value) -> Result<(), String> {
        let body = msg.to_string();
        let mut stdin = self.stdin.lock().await;
        stdin
            .write_all(format!("Content-Length: {}\r\n\r\n", body.len()).as_bytes())
            .await
            .map_err(|e| e.to_string())?;
        stdin
            .write_all(body.as_bytes())
            .await
            .map_err(|e| e.to_string())?;
        stdin.flush().await.map_err(|e| e.to_string())
    }

    pub async fn request(&self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);
        self.send(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))
            .await?;
        rx.await.map_err(|_| "server closed".to_string())?
    }

    pub async fn notify(&self, method: &str, params: Value) -> Result<(), String> {
        self.send(json!({ "jsonrpc": "2.0", "method": method, "params": params }))
            .await
    }
}
