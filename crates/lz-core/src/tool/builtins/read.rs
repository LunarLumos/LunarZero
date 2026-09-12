//! `read` tool: files with line numbers (offset/limit), directory listings,
//! images/PDFs as attachments.

use std::borrow::Cow;
use std::io::Read;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use lz_schema::session::FilePart;
use serde::Deserialize;
use serde_json::{Value, json};

use super::external_directory;
use crate::tool::{Tool, ToolCtx, ToolError, ToolResult, parse_args};

const DEFAULT_READ_LIMIT: usize = 2000;
const MAX_LINE_LENGTH: usize = 2000;
const MAX_LINE_SUFFIX: &str = "... (line truncated to 2000 chars)";
const MAX_BYTES: usize = 50 * 1024;
const SAMPLE_BYTES: usize = 4096;
const IMAGE_MIMES: &[&str] = &["image/jpeg", "image/png", "image/gif", "image/webp"];
const BINARY_EXTS: &[&str] = &[
    "zip", "tar", "gz", "exe", "dll", "so", "class", "jar", "war", "7z", "doc", "docx", "xls", "xlsx", "ppt",
    "pptx", "odt", "ods", "odp", "bin", "dat", "obj", "o", "a", "lib", "wasm", "pyc", "pyo",
];

#[derive(Deserialize)]
struct Args {
    #[serde(rename = "filePath")]
    file_path: String,
    #[serde(default)]
    offset: Option<usize>,
    #[serde(default)]
    limit: Option<usize>,
    /// Outline only: signatures, types, fields, doc comments; function bodies elided.
    #[serde(default)]
    skeleton: bool,
}

pub struct ReadTool;

pub fn list_dir(path: &Path) -> std::io::Result<String> {
    let mut items: Vec<String> = std::fs::read_dir(path)?
        .flatten()
        .map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            let is_dir = e.path().is_dir();
            if is_dir { format!("{name}/") } else { name }
        })
        .collect();
    items.sort();
    Ok(items.join("\n"))
}

pub fn is_binary(path: &Path, sample: &[u8]) -> bool {
    if let Some(ext) = path.extension().and_then(|e| e.to_str())
        && BINARY_EXTS.contains(&ext.to_lowercase().as_str())
    {
        return true;
    }
    if sample.is_empty() {
        return false;
    }
    let mut non_printable = 0usize;
    for &b in sample {
        if b == 0 {
            return true;
        }
        if b < 9 || (b > 13 && b < 32) {
            non_printable += 1;
        }
    }
    non_printable as f64 / sample.len() as f64 > 0.3
}

pub fn sniff_mime(path: &Path, sample: &[u8]) -> String {
    if let Some(kind) = infer::get(sample) {
        return kind.mime_type().to_string();
    }
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase())
        .as_deref()
    {
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("pdf") => "application/pdf",
        Some("md") => "text/markdown",
        Some("json") => "application/json",
        _ => "text/plain",
    }
    .to_string()
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

pub struct Lines {
    pub raw: Vec<String>,
    pub count: usize,
    pub cut: bool,
    pub more: bool,
}

pub fn read_lines(path: &Path, offset: usize, limit: usize) -> std::io::Result<Lines> {
    let text = std::fs::read_to_string(path)?;
    let start = offset.saturating_sub(1);
    let mut raw = Vec::new();
    let mut bytes = 0usize;
    let mut count = 0usize;
    let mut cut = false;
    let mut more = false;
    for line in text.split('\n') {
        count += 1;
        if count <= start {
            continue;
        }
        if raw.len() >= limit {
            more = true;
            continue;
        }
        let line = if line.chars().count() > MAX_LINE_LENGTH {
            format!(
                "{}{MAX_LINE_SUFFIX}",
                line.chars().take(MAX_LINE_LENGTH).collect::<String>()
            )
        } else {
            line.to_string()
        };
        let size = line.len() + usize::from(!raw.is_empty());
        if bytes + size <= MAX_BYTES {
            bytes += size;
            raw.push(line);
        } else {
            cut = true;
            more = true;
            break;
        }
    }
    // a trailing newline yields an empty final "line" — drop it like `split("\n")` semantics in JS keep it;
    Ok(Lines {
        raw,
        count,
        cut,
        more,
    })
}

pub enum PromptRead {
    Text(String),
    Binary { mime: String, data_url: String },
}

/// Read a file attached to a prompt: text → numbered content; images/pdf → data URL.
pub fn read_for_prompt(path: &Path) -> Result<PromptRead, String> {
    let meta = std::fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut sample = vec![0u8; SAMPLE_BYTES.min(meta.len() as usize)];
    if !sample.is_empty() {
        let mut f = std::fs::File::open(path).map_err(|e| e.to_string())?;
        let n = f.read(&mut sample).map_err(|e| e.to_string())?;
        sample.truncate(n);
    }
    let mime = sniff_mime(path, &sample);
    if IMAGE_MIMES.contains(&mime.as_str()) || mime == "application/pdf" {
        let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
        return Ok(PromptRead::Binary {
            mime: mime.clone(),
            data_url: format!("data:{mime};base64,{}", base64_encode(&bytes)),
        });
    }
    if is_binary(path, &sample) {
        return Err(format!("Cannot read binary file: {}", path.display()));
    }
    let lines = read_lines(path, 1, DEFAULT_READ_LIMIT).map_err(|e| e.to_string())?;
    Ok(PromptRead::Text(render(path, &lines, 1)))
}

fn render(path: &Path, file: &Lines, offset: usize) -> String {
    let mut output = format!("<path>{}</path>\n<type>file</type>\n<content>\n", path.display());
    output.push_str(
        &file
            .raw
            .iter()
            .enumerate()
            .map(|(i, l)| format!("{}: {l}", i + offset))
            .collect::<Vec<_>>()
            .join("\n"),
    );
    let last = offset + file.raw.len().saturating_sub(1);
    let next = last + 1;
    if file.cut {
        output.push_str(&format!(
            "\n\n(Output capped at 50 KB. Showing lines {offset}-{last}. Use offset={next} to continue.)"
        ));
    } else if file.more {
        output.push_str(&format!(
            "\n\n(Showing lines {offset}-{last} of {}. Use offset={next} to continue.)",
            file.count
        ));
    } else {
        output.push_str(&format!("\n\n(End of file - total {} lines)", file.count));
    }
    output.push_str("\n</content>");
    output
}

fn suggest(path: &Path) -> String {
    let dir = path.parent().unwrap_or(Path::new("."));
    let base = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_lowercase();
    let mut items: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .filter(|e| {
                    let n = e.file_name().to_string_lossy().to_lowercase();
                    n.contains(&base) || base.contains(&n)
                })
                .map(|e| e.path())
                .collect()
        })
        .unwrap_or_default();
    items.truncate(3);
    if items.is_empty() {
        format!("File not found: {}", path.display())
    } else {
        format!(
            "File not found: {}\n\nDid you mean one of these?\n{}",
            path.display(),
            items
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join("\n")
        )
    }
}

#[async_trait]
impl Tool for ReadTool {
    fn id(&self) -> &'static str {
        "read"
    }
    fn description(&self) -> Cow<'static, str> {
        Cow::Borrowed(crate::tool_description!("read"))
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "filePath": { "type": "string", "description": "Absolute path" },
                "offset": { "type": "integer", "description": "First line (1-based)" },
                "limit": { "type": "integer", "description": "Max lines (default 2000)" },
                "skeleton": { "type": "boolean", "description": "Outline only (signatures, types, fields, docs; bodies elided) — ~10x fewer tokens; Rust/Python/JS/TS/Go" }
            },
            "required": ["filePath"]
        })
    }

    async fn execute(&self, ctx: ToolCtx, args: Value) -> Result<ToolResult, ToolError> {
        let args: Args = parse_args(args)?;
        let path = ctx.engine.resolve_path(&args.file_path);
        let title = path
            .strip_prefix(ctx.worktree())
            .unwrap_or(&path)
            .display()
            .to_string();
        let meta = std::fs::metadata(&path).ok();
        let is_dir = meta.as_ref().is_some_and(|m| m.is_dir());
        external_directory::assert(&ctx, &path, is_dir).await?;
        ctx.ask("read", vec![title.clone()], vec!["*".into()], Default::default())
            .await?;
        let Some(meta) = meta else {
            return Err(ToolError::Other(suggest(&path)));
        };

        if is_dir {
            let mut items: Vec<String> = list_dir(&path)
                .map_err(ToolError::other)?
                .lines()
                .map(str::to_string)
                .collect();
            let limit = args.limit.unwrap_or(DEFAULT_READ_LIMIT);
            let offset = args.offset.unwrap_or(1).max(1);
            let total = items.len();
            let start = (offset - 1).min(total);
            items = items.into_iter().skip(start).take(limit).collect();
            let truncated = start + items.len() < total;
            let note = if truncated {
                format!(
                    "\n(Showing {} of {total} entries. Use 'offset' parameter to read beyond entry {})",
                    items.len(),
                    offset + items.len()
                )
            } else {
                format!("\n({total} entries)")
            };
            let output = format!(
                "<path>{}</path>\n<type>directory</type>\n<entries>\n{}{note}\n</entries>",
                path.display(),
                items.join("\n")
            );
            return Ok(ToolResult {
                title,
                output,
                metadata: json!({ "preview": items.iter().take(20).cloned().collect::<Vec<_>>().join("\n"), "truncated": truncated, "loaded": [] }),
                attachments: Vec::new(),
            });
        }

        let loaded = ctx
            .engine
            .resolve_nested_instructions(&ctx.messages, &path, &ctx.message_id)
            .await;
        let mut sample = vec![0u8; SAMPLE_BYTES.min(meta.len() as usize)];
        if !sample.is_empty() {
            let mut f = std::fs::File::open(&path).map_err(ToolError::other)?;
            let n = f.read(&mut sample).map_err(ToolError::other)?;
            sample.truncate(n);
        }
        let mime = sniff_mime(&path, &sample);
        if IMAGE_MIMES.contains(&mime.as_str()) || mime == "application/pdf" {
            let bytes = std::fs::read(&path).map_err(ToolError::other)?;
            let msg = if mime == "application/pdf" {
                "PDF read successfully"
            } else {
                "Image read successfully"
            };
            return Ok(ToolResult {
                title,
                output: msg.into(),
                metadata: json!({ "preview": msg, "truncated": false, "loaded": loaded.iter().map(|(p, _)| p.display().to_string()).collect::<Vec<_>>() }),
                attachments: vec![FilePart {
                    id: lz_schema::ids::ascending(lz_schema::ids::Prefix::Part),
                    session_id: ctx.session_id.clone(),
                    message_id: ctx.message_id.clone(),
                    mime: mime.clone(),
                    filename: path.file_name().map(|n| n.to_string_lossy().to_string()),
                    url: format!("data:{mime};base64,{}", base64_encode(&bytes)),
                    source: None,
                }],
            });
        }
        if is_binary(&path, &sample) {
            return Err(ToolError::Other(format!(
                "Cannot read binary file: {}",
                path.display()
            )));
        }
        if args.skeleton {
            let src = std::fs::read_to_string(&path).map_err(ToolError::other)?;
            let Some(sk) = crate::index::skeleton(&path, &src) else {
                return Err(ToolError::Invalid(
                    "skeleton is only available for Rust, Python, JavaScript/TypeScript and Go files".into(),
                ));
            };
            let total = src.matches('\n').count() + 1;
            let output = format!(
                "<file>\n{sk}\n</file>\n({total} lines; bodies elided — read with offset/limit for one)"
            );
            ctx.engine.lsp_touch(&path).await;
            return Ok(ToolResult {
                title: format!("{title} (skeleton)"),
                metadata: json!({ "preview": sk.lines().take(20).collect::<Vec<_>>().join("\n"), "truncated": false, "loaded": [], "skeleton": true }),
                output,
                attachments: Vec::new(),
            });
        }
        let offset = args.offset.unwrap_or(1).max(1);
        let limit = args.limit.unwrap_or(DEFAULT_READ_LIMIT);
        let file = read_lines(&path, offset, limit).map_err(ToolError::other)?;
        if file.count < offset && !(file.count == 0 && offset == 1) {
            return Err(ToolError::Other(format!(
                "Offset {offset} is out of range for this file ({} lines)",
                file.count
            )));
        }
        let mut output = render(&path, &file, offset);
        if args.offset.is_none() && args.limit.is_none() && file.count > 400 && crate::index::supports(&path)
        {
            output.push_str(
                "\n(tip: `skeleton: true` returns this file's outline in a fraction of the tokens)",
            );
        }
        ctx.engine.lsp_touch(&path).await;
        if !loaded.is_empty() {
            output.push_str(&format!(
                "\n\n<system-reminder>\n{}\n</system-reminder>",
                loaded
                    .iter()
                    .map(|(_, c)| c.as_str())
                    .collect::<Vec<_>>()
                    .join("\n\n")
            ));
        }
        let last = offset + file.raw.len().saturating_sub(1);
        Ok(ToolResult {
            title,
            metadata: json!({
                "preview": file.raw.iter().take(20).cloned().collect::<Vec<_>>().join("\n"),
                "truncated": file.more || file.cut,
                "loaded": loaded.iter().map(|(p, _)| p.display().to_string()).collect::<Vec<_>>(),
                "display": { "type": "file", "path": path.display().to_string(), "text": file.raw.join("\n"),
                             "lineStart": offset, "lineEnd": last, "totalLines": file.count, "truncated": file.more || file.cut }
            }),
            output,
            attachments: Vec::new(),
        })
    }
}
