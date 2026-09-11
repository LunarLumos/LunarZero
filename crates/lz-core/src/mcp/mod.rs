//! MCP client manager: connects configured servers (local stdio or remote
//! streamable-HTTP), exposes their tools as `<server>_<tool>` LunarZero tools,
//! and their prompts as slash commands.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use lz_schema::Event;
use lz_schema::api::McpStatus;
use lz_schema::config::{McpEntry, McpServerConfig};
use lz_schema::session::FilePart;
use rmcp::RoleClient;
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, ClientCapabilities, ClientInfo, ContentBlock, Implementation};
use rmcp::service::RunningService;
use serde_json::{Value, json};
use tokio::sync::RwLock;

use crate::tool::{Tool, ToolCtx, ToolError, ToolResult};

type Client = RunningService<RoleClient, ClientInfo>;

pub struct McpServer {
    pub name: String,
    pub config: McpServerConfig,
    pub status: McpStatus,
    client: Option<Arc<Client>>,
    pub tools: Vec<rmcp::model::Tool>,
    pub prompts: Vec<rmcp::model::Prompt>,
    pub instructions: Option<String>,
}

#[derive(Default)]
pub struct McpManager {
    servers: RwLock<BTreeMap<String, McpServer>>,
    pub default_timeout: Duration,
}

/// `name` → tool-safe identifier (`[a-zA-Z0-9_-]`).
pub fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn client_info() -> ClientInfo {
    ClientInfo::new(
        ClientCapabilities::default(),
        Implementation::new("lunarzero", env!("CARGO_PKG_VERSION")),
    )
}

async fn connect(
    name: &str,
    cfg: &McpServerConfig,
    cwd: &std::path::Path,
    timeout: Duration,
) -> Result<Client, String> {
    match cfg {
        McpServerConfig::Local {
            command,
            cwd: server_cwd,
            environment,
            ..
        } => {
            let Some((bin, args)) = command.split_first() else {
                return Err("empty command".into());
            };
            let mut cmd = tokio::process::Command::new(bin);
            cmd.args(args);
            cmd.current_dir(
                server_cwd
                    .as_deref()
                    .map(|c| cwd.join(c))
                    .unwrap_or_else(|| cwd.to_path_buf()),
            );
            if let Some(env) = environment {
                for (k, v) in env {
                    if let Some(v) = v.as_str() {
                        cmd.env(k, v);
                    }
                }
            }
            // capture stderr: servers print banners and tracebacks there, which
            // must never land on the user's terminal; keep the tail for errors
            let (transport, stderr) = rmcp::transport::TokioChildProcess::builder(cmd)
                .stderr(std::process::Stdio::piped())
                .spawn()
                .map_err(|e| format!("spawn failed: {e}"))?;
            let tail: Arc<std::sync::Mutex<std::collections::VecDeque<String>>> =
                Arc::new(std::sync::Mutex::new(std::collections::VecDeque::new()));
            if let Some(err) = stderr {
                let tail = tail.clone();
                let server = name.to_string();
                tokio::spawn(async move {
                    use tokio::io::AsyncBufReadExt;
                    let mut lines = tokio::io::BufReader::new(err).lines();
                    while let Ok(Some(line)) = lines.next_line().await {
                        tracing::debug!(mcp = %server, "{line}");
                        let mut t = tail.lock().unwrap();
                        if t.len() >= 12 {
                            t.pop_front();
                        }
                        t.push_back(line);
                    }
                });
            }
            let result = tokio::time::timeout(timeout, client_info().serve(transport)).await;
            let explain = || {
                let t = tail.lock().unwrap();
                // the last meaningful line is usually the real reason (e.g. a Python exception)
                t.iter()
                    .rev()
                    .find(|l| !l.trim().is_empty() && !l.starts_with("  "))
                    .cloned()
                    .map(|l| format!(" — {}", l.chars().take(200).collect::<String>()))
                    .unwrap_or_default()
            };
            match result {
                Err(_) => Err(format!("timed out connecting to {name}{}", explain())),
                Ok(Err(e)) => {
                    // give stderr a moment to arrive
                    tokio::time::sleep(Duration::from_millis(150)).await;
                    Err(format!("initialize failed: {e}{}", explain()))
                }
                Ok(Ok(client)) => Ok(client),
            }
        }
        McpServerConfig::Remote { url, headers, .. } => {
            let mut config =
                rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig::with_uri(
                    url.clone(),
                );
            if let Some(h) = headers {
                for (k, v) in h {
                    if let (Ok(name), Some(v)) =
                        (reqwest::header::HeaderName::from_bytes(k.as_bytes()), v.as_str())
                        && let Ok(val) = reqwest::header::HeaderValue::from_str(v)
                    {
                        if name == reqwest::header::AUTHORIZATION {
                            config.auth_header = Some(v.trim_start_matches("Bearer ").to_string());
                        } else {
                            config.custom_headers.insert(name, val);
                        }
                    }
                }
            }
            let transport =
                rmcp::transport::StreamableHttpClientTransport::with_client(reqwest::Client::new(), config);
            let client = tokio::time::timeout(timeout, client_info().serve(transport))
                .await
                .map_err(|_| format!("timed out connecting to {name}"))?
                .map_err(|e| format!("initialize failed: {e}"))?;
            Ok(client)
        }
    }
}

impl McpManager {
    pub fn new(timeout: Duration) -> Self {
        Self {
            servers: RwLock::new(BTreeMap::new()),
            default_timeout: timeout,
        }
    }

    /// (Re)load from config: connect enabled servers concurrently.
    pub async fn load(
        &self,
        entries: &BTreeMap<String, McpEntry>,
        cwd: &std::path::Path,
        bus: &crate::bus::Bus,
    ) {
        let mut servers = BTreeMap::new();
        let mut handles = Vec::new();
        for (name, entry) in entries {
            let McpEntry::Server(cfg) = entry else {
                servers.insert(
                    name.clone(),
                    McpServer {
                        name: name.clone(),
                        config: McpServerConfig::Local {
                            command: Vec::new(),
                            cwd: None,
                            environment: None,
                            enabled: Some(false),
                            timeout: None,
                        },
                        status: McpStatus::Disabled,
                        client: None,
                        tools: Vec::new(),
                        prompts: Vec::new(),
                        instructions: None,
                    },
                );
                continue;
            };
            let enabled = match cfg {
                McpServerConfig::Local { enabled, .. } | McpServerConfig::Remote { enabled, .. } => {
                    enabled.unwrap_or(true)
                }
            };
            if !enabled {
                servers.insert(
                    name.clone(),
                    McpServer {
                        name: name.clone(),
                        config: cfg.clone(),
                        status: McpStatus::Disabled,
                        client: None,
                        tools: Vec::new(),
                        prompts: Vec::new(),
                        instructions: None,
                    },
                );
                continue;
            }
            let timeout = match cfg {
                McpServerConfig::Local { timeout, .. } | McpServerConfig::Remote { timeout, .. } => {
                    timeout.map(Duration::from_millis).unwrap_or(self.default_timeout)
                }
            };
            let (name, cfg, cwd) = (name.clone(), cfg.clone(), cwd.to_path_buf());
            handles.push(tokio::spawn(async move {
                let result = connect(&name, &cfg, &cwd, timeout).await;
                (name, cfg, result)
            }));
        }
        for h in handles {
            let Ok((name, cfg, result)) = h.await else {
                continue;
            };
            let server = match result {
                Ok(client) => {
                    let tools = client.list_all_tools().await.unwrap_or_default();
                    let prompts = client.list_all_prompts().await.unwrap_or_default();
                    let instructions = client.peer_info().and_then(|i| i.instructions.clone());
                    tracing::info!(server = name, tools = tools.len(), "mcp connected");
                    McpServer {
                        name: name.clone(),
                        config: cfg,
                        status: McpStatus::Connected,
                        client: Some(Arc::new(client)),
                        tools,
                        prompts,
                        instructions,
                    }
                }
                Err(e) => {
                    tracing::warn!(server = name, "mcp connect failed: {e}");
                    let status = if e.contains("401") || e.to_lowercase().contains("unauthorized") {
                        McpStatus::NeedsAuth
                    } else {
                        McpStatus::Failed { error: e }
                    };
                    McpServer {
                        name: name.clone(),
                        config: cfg,
                        status,
                        client: None,
                        tools: Vec::new(),
                        prompts: Vec::new(),
                        instructions: None,
                    }
                }
            };
            bus.publish(Event::McpStatus {
                name: name.clone(),
                status: server.status.clone(),
            });
            servers.insert(name, server);
        }
        *self.servers.write().await = servers;
    }

    pub async fn status(&self) -> BTreeMap<String, McpStatus> {
        self.servers
            .read()
            .await
            .iter()
            .map(|(k, v)| (k.clone(), v.status.clone()))
            .collect()
    }

    pub async fn instructions(&self) -> Vec<(String, String, Vec<String>)> {
        self.servers
            .read()
            .await
            .values()
            .filter_map(|s| {
                s.instructions.as_ref().map(|i| {
                    (
                        s.name.clone(),
                        i.clone(),
                        s.tools
                            .iter()
                            .map(|t| format!("{}_{}", sanitize(&s.name), sanitize(&t.name)))
                            .collect(),
                    )
                })
            })
            .collect()
    }

    /// Per connected server: (name, tool ids, searchable text = name + instructions + tool names/descriptions).
    pub async fn index(&self) -> Vec<(String, Vec<String>, String)> {
        let servers = self.servers.read().await;
        servers
            .values()
            .filter(|s| s.client.is_some())
            .map(|s| {
                let ids: Vec<String> = s
                    .tools
                    .iter()
                    .map(|t| format!("{}_{}", sanitize(&s.name), sanitize(&t.name)))
                    .collect();
                let mut text = format!("{} {} ", s.name, s.instructions.clone().unwrap_or_default());
                for t in &s.tools {
                    text.push_str(&t.name);
                    text.push(' ');
                    if let Some(d) = &t.description {
                        text.push_str(d);
                        text.push(' ');
                    }
                }
                (s.name.clone(), ids, text)
            })
            .collect()
    }

    /// LunarZero tool wrappers for every connected server's tools.
    pub async fn tools(&self) -> Vec<Arc<dyn Tool>> {
        let servers = self.servers.read().await;
        let mut out: Vec<Arc<dyn Tool>> = Vec::new();
        for s in servers.values() {
            let Some(client) = &s.client else { continue };
            for t in &s.tools {
                let mut schema = Value::Object((*t.input_schema).clone());
                if let Value::Object(m) = &mut schema {
                    m.entry("type").or_insert(json!("object"));
                    m.entry("additionalProperties").or_insert(json!(false));
                }
                out.push(Arc::new(McpTool {
                    id: format!("{}_{}", sanitize(&s.name), sanitize(&t.name)),
                    server: s.name.clone(),
                    tool: t.name.to_string(),
                    description: t.description.clone().map(|d| d.to_string()).unwrap_or_default(),
                    schema,
                    client: client.clone(),
                }));
            }
        }
        out
    }

    /// Prompts as (name, description, arg names, resolved template).
    pub async fn prompts(&self) -> Vec<(String, Option<String>, Vec<String>, Arc<Client>, String)> {
        let servers = self.servers.read().await;
        let mut out = Vec::new();
        for s in servers.values() {
            let Some(client) = &s.client else { continue };
            for p in &s.prompts {
                let args: Vec<String> = p
                    .arguments
                    .as_ref()
                    .map(|a| a.iter().map(|x| x.name.clone()).collect())
                    .unwrap_or_default();
                out.push((
                    p.name.clone(),
                    p.description.clone(),
                    args,
                    client.clone(),
                    s.name.clone(),
                ));
            }
        }
        out
    }

    pub async fn get_prompt(
        &self,
        client: &Client,
        name: &str,
        args: BTreeMap<String, String>,
    ) -> Option<String> {
        let params = rmcp::model::GetPromptRequestParams::new(name)
            .with_arguments(args.into_iter().map(|(k, v)| (k, Value::String(v))).collect());
        let r = client.get_prompt(params).await.ok()?;
        Some(
            r.messages
                .iter()
                .filter_map(|m| match &m.content {
                    ContentBlock::Text(t) => Some(t.text.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n"),
        )
    }

    pub async fn connect_one(
        &self,
        name: &str,
        cwd: &std::path::Path,
        bus: &crate::bus::Bus,
    ) -> Result<(), String> {
        let cfg = { self.servers.read().await.get(name).map(|s| s.config.clone()) };
        let Some(cfg) = cfg else {
            return Err(format!("unknown mcp server {name}"));
        };
        let client = connect(name, &cfg, cwd, self.default_timeout).await?;
        let tools = client.list_all_tools().await.unwrap_or_default();
        let prompts = client.list_all_prompts().await.unwrap_or_default();
        let instructions = client.peer_info().and_then(|i| i.instructions.clone());
        let mut servers = self.servers.write().await;
        if let Some(s) = servers.get_mut(name) {
            s.client = Some(Arc::new(client));
            s.tools = tools;
            s.prompts = prompts;
            s.instructions = instructions;
            s.status = McpStatus::Connected;
        }
        bus.publish(Event::McpStatus {
            name: name.into(),
            status: McpStatus::Connected,
        });
        Ok(())
    }

    pub async fn disconnect_one(&self, name: &str, bus: &crate::bus::Bus) -> Result<(), String> {
        let mut servers = self.servers.write().await;
        let s = servers
            .get_mut(name)
            .ok_or_else(|| format!("unknown mcp server {name}"))?;
        s.client = None;
        s.tools.clear();
        s.prompts.clear();
        s.status = McpStatus::Disabled;
        bus.publish(Event::McpStatus {
            name: name.into(),
            status: McpStatus::Disabled,
        });
        Ok(())
    }

    pub async fn shutdown(&self) {
        let mut servers = self.servers.write().await;
        for s in servers.values_mut() {
            if let Some(c) = s.client.take()
                && let Ok(c) = Arc::try_unwrap(c)
            {
                let _ = c.cancel().await;
            }
        }
    }
}

struct McpTool {
    id: String,
    server: String,
    tool: String,
    description: String,
    schema: Value,
    client: Arc<Client>,
}

#[async_trait]
impl Tool for McpTool {
    fn id(&self) -> &'static str {
        Box::leak(self.id.clone().into_boxed_str())
    }
    fn description(&self) -> Cow<'static, str> {
        Cow::Owned(self.description.clone())
    }
    fn parameters(&self) -> Value {
        self.schema.clone()
    }
    async fn execute(&self, ctx: ToolCtx, args: Value) -> Result<ToolResult, ToolError> {
        ctx.ask(
            &self.id,
            vec!["*".into()],
            vec!["*".into()],
            json!({ "server": self.server, "tool": self.tool, "input": args })
                .as_object()
                .cloned()
                .unwrap_or_default(),
        )
        .await?;
        let params = CallToolRequestParams::new(self.tool.clone())
            .with_arguments(args.as_object().cloned().unwrap_or_default());
        let call = self.client.call_tool(params);
        let result = tokio::select! {
            r = call => r.map_err(|e| ToolError::Other(format!("MCP tool {} failed: {e}", self.id)))?,
            _ = ctx.cancel.cancelled() => return Err(ToolError::Aborted),
        };
        let mut texts = Vec::new();
        let mut attachments = Vec::new();
        for c in &result.content {
            match c {
                ContentBlock::Text(t) => texts.push(t.text.clone()),
                ContentBlock::Image(i) => attachments.push(FilePart {
                    id: lz_schema::ids::ascending(lz_schema::ids::Prefix::Part),
                    session_id: ctx.session_id.clone(),
                    message_id: ctx.message_id.clone(),
                    mime: i.mime_type.clone(),
                    filename: None,
                    url: format!("data:{};base64,{}", i.mime_type, i.data),
                    source: None,
                }),
                other => texts.push(serde_json::to_string(other).unwrap_or_default()),
            }
        }
        if texts.is_empty()
            && let Some(s) = &result.structured_content
        {
            texts.push(s.to_string());
        }
        let output = texts.join("\n");
        if result.is_error == Some(true) {
            return Err(ToolError::Other(if output.is_empty() {
                "MCP tool returned an error".into()
            } else {
                output
            }));
        }
        Ok(ToolResult {
            title: self.id.clone(),
            output,
            metadata: json!({}),
            attachments,
        })
    }
}
