//! HTTP execution of a [`Protocol`]: POST the body, stream SSE, map errors.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::protocol::Protocol;
use super::sse::SseParser;
use super::types::*;

/// Where and how to reach a provider endpoint.
#[derive(Debug, Clone)]
pub struct Endpoint {
    pub base_url: String,
    pub api_key: Option<String>,
    pub headers: Vec<(String, String)>,
    /// Time to wait for response headers (default 300 s).
    pub header_timeout: Duration,
    /// Max silence between SSE chunks (default 300 s).
    pub chunk_timeout: Duration,
}

impl Endpoint {
    pub fn new(base_url: impl Into<String>, api_key: Option<String>) -> Self {
        Self {
            base_url: base_url.into(),
            api_key,
            headers: Vec::new(),
            header_timeout: Duration::from_secs(300),
            chunk_timeout: Duration::from_secs(300),
        }
    }
}

#[derive(Clone)]
pub struct LlmClient {
    http: reqwest::Client,
}

impl Default for LlmClient {
    fn default() -> Self {
        Self::new()
    }
}

impl LlmClient {
    pub fn new() -> Self {
        let http = reqwest::Client::builder()
            .user_agent(format!("lunarzero/{}", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(30))
            .build()
            .expect("reqwest client");
        Self { http }
    }

    /// Start a streaming request. Events arrive on the returned receiver; the
    /// stream ends with `Ok(Finish)` or an `Err`.
    pub fn stream(
        &self,
        protocol: Arc<dyn Protocol>,
        endpoint: Endpoint,
        req: LlmRequest,
        cancel: CancellationToken,
    ) -> mpsc::Receiver<Result<LlmEvent, LlmError>> {
        let (tx, rx) = mpsc::channel(256);
        let http = self.http.clone();
        tokio::spawn(async move {
            let result = run(http, protocol, endpoint, req, cancel, &tx).await;
            if let Err(e) = result {
                let _ = tx.send(Err(e)).await;
            }
        });
        rx
    }
}

fn parse_retry_after(headers: &reqwest::header::HeaderMap) -> Option<u64> {
    if let Some(v) = headers.get("retry-after-ms").and_then(|v| v.to_str().ok())
        && let Ok(ms) = v.trim().parse::<u64>()
    {
        return Some(ms);
    }
    let v = headers.get("retry-after")?.to_str().ok()?.trim();
    if let Ok(secs) = v.parse::<f64>() {
        return Some((secs * 1000.0) as u64);
    }
    // HTTP-date
    let when = jiff::fmt::rfc2822::parse(v).ok()?;
    let now = jiff::Timestamp::now();
    let delta = when.timestamp().duration_since(now);
    Some(delta.as_millis().max(0) as u64)
}

fn classify_http_error(status: u16, headers: &reqwest::header::HeaderMap, body: String) -> LlmError {
    let message = serde_json::from_str::<serde_json::Value>(&body)
        .ok()
        .and_then(|v| {
            v.get("error")
                .and_then(|e| {
                    e.get("message")
                        .and_then(|m| m.as_str())
                        .map(str::to_string)
                        .or_else(|| e.as_str().map(str::to_string))
                })
                .or_else(|| v.get("message").and_then(|m| m.as_str()).map(str::to_string))
        })
        .unwrap_or_else(|| {
            if body.is_empty() {
                format!("HTTP {status}")
            } else {
                body.chars().take(500).collect()
            }
        });
    let lower = message.to_lowercase();
    let overflow = lower.contains("context length")
        || lower.contains("context window")
        || lower.contains("maximum context")
        || lower.contains("too many tokens")
        || lower.contains("prompt is too long")
        || lower.contains("input is too long")
        || lower.contains("exceeds the model");
    if overflow && matches!(status, 400 | 413 | 422) {
        return LlmError::ContextOverflow { message };
    }
    match status {
        401 | 403 => LlmError::Authentication { message },
        429 => LlmError::RateLimited {
            message,
            retry_after_ms: parse_retry_after(headers),
        },
        _ => LlmError::Provider {
            status,
            message,
            retry_after_ms: parse_retry_after(headers),
            headers: headers
                .iter()
                .filter_map(|(k, v)| v.to_str().ok().map(|v| (k.to_string(), v.to_string())))
                .collect::<BTreeMap<_, _>>(),
            body: Some(body),
        },
    }
}

async fn run(
    http: reqwest::Client,
    protocol: Arc<dyn Protocol>,
    endpoint: Endpoint,
    req: LlmRequest,
    cancel: CancellationToken,
    tx: &mpsc::Sender<Result<LlmEvent, LlmError>>,
) -> Result<(), LlmError> {
    let wire = protocol.build(&req)?;
    let url = format!("{}{}", endpoint.base_url.trim_end_matches('/'), wire.path);
    let mut builder = http
        .post(&url)
        .json(&wire.body)
        .header("accept", "text/event-stream");
    if let Some(key) = &endpoint.api_key {
        builder = builder.bearer_auth(key);
    }
    for (k, v) in endpoint.headers.iter().chain(req.headers.iter()) {
        builder = builder.header(k, v);
    }
    tracing::debug!(url, model = req.model_id, "llm request");

    let response = tokio::select! {
        _ = cancel.cancelled() => return Err(LlmError::Aborted),
        r = tokio::time::timeout(endpoint.header_timeout, builder.send()) => match r {
            Err(_) => return Err(LlmError::Timeout { message: "waiting for response headers".into() }),
            Ok(Err(e)) => return Err(LlmError::Network { message: e.to_string() }),
            Ok(Ok(resp)) => resp,
        },
    };

    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        let headers = response.headers().clone();
        let body = response.text().await.unwrap_or_default();
        return Err(classify_http_error(status, &headers, body));
    }

    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let mut parser = protocol.parser();

    // Some servers ignore `stream: true` and return a JSON body; treat it as one event.
    if content_type.starts_with("application/json") {
        let body = response.text().await.map_err(|e| LlmError::Network {
            message: e.to_string(),
        })?;
        for ev in parser.step(&body)? {
            if tx.send(Ok(ev)).await.is_err() {
                return Ok(());
            }
        }
        for ev in parser.halt()? {
            if tx.send(Ok(ev)).await.is_err() {
                return Ok(());
            }
        }
        return Ok(());
    }

    let mut sse = SseParser::new();
    let mut body = response.bytes_stream();
    loop {
        let chunk = tokio::select! {
            _ = cancel.cancelled() => return Err(LlmError::Aborted),
            r = tokio::time::timeout(endpoint.chunk_timeout, body.next()) => match r {
                Err(_) => return Err(LlmError::Timeout { message: "no data from provider".into() }),
                Ok(None) => break,
                Ok(Some(Err(e))) => return Err(LlmError::Network { message: e.to_string() }),
                Ok(Some(Ok(bytes))) => bytes,
            },
        };
        let messages = sse
            .push(&chunk)
            .map_err(|m| LlmError::InvalidOutput { message: m })?;
        for msg in messages {
            for ev in parser.step(&msg.data)? {
                if tx.send(Ok(ev)).await.is_err() {
                    return Ok(());
                }
            }
        }
    }
    if let Some(msg) = sse.finish() {
        for ev in parser.step(&msg.data)? {
            if tx.send(Ok(ev)).await.is_err() {
                return Ok(());
            }
        }
    }
    for ev in parser.halt()? {
        if tx.send(Ok(ev)).await.is_err() {
            return Ok(());
        }
    }
    Ok(())
}
