//! `webfetch` — GET a URL and return markdown / text / html (5 MB cap).

use std::borrow::Cow;
use std::time::Duration;

use async_trait::async_trait;
use lz_schema::session::FilePart;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::tool::{Tool, ToolCtx, ToolError, ToolResult, parse_args};

const MAX_RESPONSE_SIZE: usize = 5 * 1024 * 1024;
const DEFAULT_TIMEOUT_S: u64 = 30;
const MAX_TIMEOUT_S: u64 = 120;

#[derive(Deserialize)]
struct Args {
    url: String,
    #[serde(default = "default_format")]
    format: String,
    #[serde(default)]
    timeout: Option<u64>,
}

fn default_format() -> String {
    "markdown".into()
}

pub struct WebFetchTool;

fn html_to_text(html: &str) -> String {
    let doc = scraper::Html::parse_document(html);
    let skip = scraper::Selector::parse("script, style, noscript, iframe, object, embed").ok();
    let mut out = String::new();
    for node in doc.root_element().descendants() {
        if let Some(text) = node.value().as_text() {
            let mut skipped = false;
            if let Some(skip) = &skip {
                let mut cur = node.parent();
                while let Some(p) = cur {
                    if let Some(el) = scraper::ElementRef::wrap(p)
                        && skip.matches(&el)
                    {
                        skipped = true;
                        break;
                    }
                    cur = p.parent();
                }
            }
            if !skipped {
                out.push_str(text);
            }
        }
    }
    out.trim().to_string()
}

fn base64_encode(bytes: &[u8]) -> String {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

#[async_trait]
impl Tool for WebFetchTool {
    fn id(&self) -> &'static str {
        "webfetch"
    }
    fn description(&self) -> Cow<'static, str> {
        Cow::Borrowed(crate::tool_description!("webfetch"))
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "url": { "type": "string", "description": "The URL to fetch content from" },
                "format": { "type": "string", "enum": ["text", "markdown", "html"], "description": "The format to return the content in (text, markdown, or html). Defaults to markdown." },
                "timeout": { "type": "integer", "description": "Optional timeout in seconds (max 120)" }
            },
            "required": ["url"]
        })
    }
    async fn execute(&self, ctx: ToolCtx, args: Value) -> Result<ToolResult, ToolError> {
        let args: Args = parse_args(args)?;
        if !args.url.starts_with("http://") && !args.url.starts_with("https://") {
            return Err(ToolError::Invalid(
                "URL must start with http:// or https://".into(),
            ));
        }
        ctx.ask(
            "webfetch",
            vec![args.url.clone()],
            vec!["*".into()],
            json!({ "url": args.url, "format": args.format, "timeout": args.timeout })
                .as_object()
                .cloned()
                .unwrap_or_default(),
        )
        .await?;
        let timeout = Duration::from_secs(args.timeout.unwrap_or(DEFAULT_TIMEOUT_S).min(MAX_TIMEOUT_S));
        let accept = match args.format.as_str() {
            "markdown" => {
                "text/markdown;q=1.0, text/x-markdown;q=0.9, text/plain;q=0.8, text/html;q=0.7, */*;q=0.1"
            }
            "text" => "text/plain;q=1.0, text/markdown;q=0.9, text/html;q=0.8, */*;q=0.1",
            "html" => {
                "text/html;q=1.0, application/xhtml+xml;q=0.9, text/plain;q=0.8, text/markdown;q=0.7, */*;q=0.1"
            }
            _ => "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
        };
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .map_err(ToolError::other)?;
        let ua = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/143.0.0.0 Safari/537.36";
        let send = |agent: &'static str| {
            client
                .get(&args.url)
                .header("User-Agent", agent)
                .header("Accept", accept)
                .header("Accept-Language", "en-US,en;q=0.9")
                .send()
        };
        let mut resp = tokio::select! {
            r = send(ua) => r.map_err(ToolError::other)?,
            _ = ctx.cancel.cancelled() => return Err(ToolError::Aborted),
        };
        if resp.status().as_u16() == 403 && resp.headers().get("cf-mitigated").is_some() {
            resp = send("lunarzero").await.map_err(ToolError::other)?;
        }
        if !resp.status().is_success() {
            return Err(ToolError::Other(format!(
                "Request failed with status code: {}",
                resp.status().as_u16()
            )));
        }
        if resp
            .content_length()
            .is_some_and(|l| l as usize > MAX_RESPONSE_SIZE)
        {
            return Err(ToolError::Other("Response too large (exceeds 5MB limit)".into()));
        }
        let content_type = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let bytes = resp.bytes().await.map_err(ToolError::other)?;
        if bytes.len() > MAX_RESPONSE_SIZE {
            return Err(ToolError::Other("Response too large (exceeds 5MB limit)".into()));
        }
        let mime = content_type.split(';').next().unwrap_or("").trim().to_lowercase();
        let title = format!("{} ({content_type})", args.url);
        if mime.starts_with("image/") {
            return Ok(ToolResult {
                title,
                output: "Image fetched successfully".into(),
                metadata: json!({}),
                attachments: vec![FilePart {
                    id: lz_schema::ids::ascending(lz_schema::ids::Prefix::Part),
                    session_id: ctx.session_id.clone(),
                    message_id: ctx.message_id.clone(),
                    mime: mime.clone(),
                    filename: None,
                    url: format!("data:{mime};base64,{}", base64_encode(&bytes)),
                    source: None,
                }],
            });
        }
        let content = String::from_utf8_lossy(&bytes).to_string();
        let is_html = content_type.contains("text/html");
        let output = match args.format.as_str() {
            "markdown" if is_html => htmd::convert(&content).unwrap_or_else(|_| html_to_text(&content)),
            "text" if is_html => html_to_text(&content),
            _ => content,
        };
        Ok(ToolResult {
            title,
            output,
            metadata: json!({}),
            attachments: Vec::new(),
        })
    }
}
