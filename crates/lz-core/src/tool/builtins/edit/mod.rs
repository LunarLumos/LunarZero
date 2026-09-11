//! `edit` and `write` tools.

pub mod replacers;

use std::borrow::Cow;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};

use async_trait::async_trait;
use lz_schema::Event;
use serde::Deserialize;
use serde_json::{Value, json};

use super::external_directory;
use crate::tool::{Tool, ToolCtx, ToolError, ToolResult, parse_args};

/// Per-path locks so concurrent edits to one file serialize.
static LOCKS: LazyLock<Mutex<HashMap<PathBuf, Arc<tokio::sync::Mutex<()>>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn lock_for(path: &Path) -> Arc<tokio::sync::Mutex<()>> {
    let mut m = LOCKS.lock().unwrap_or_else(|e| e.into_inner());
    m.entry(path.to_path_buf())
        .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
        .clone()
}

const BOM: char = '\u{feff}';

fn split_bom(s: &str) -> (bool, &str) {
    match s.strip_prefix(BOM) {
        Some(rest) => (true, rest),
        None => (false, s),
    }
}

fn detect_line_ending(s: &str) -> &'static str {
    if s.contains("\r\n") { "\r\n" } else { "\n" }
}

fn normalize_line_endings(s: &str) -> String {
    s.replace("\r\n", "\n")
}

fn to_line_ending(s: &str, ending: &str) -> String {
    if ending == "\r\n" {
        normalize_line_endings(s).replace('\n', "\r\n")
    } else {
        normalize_line_endings(s)
    }
}

/// Unified diff with the common leading indentation stripped from content
/// lines so permission prompts stay narrow.
pub fn trim_diff(diff: &str) -> String {
    let is_content = |l: &str| {
        (l.starts_with('+') || l.starts_with('-') || l.starts_with(' '))
            && !l.starts_with("---")
            && !l.starts_with("+++")
    };
    let min = diff
        .lines()
        .filter(|l| is_content(l))
        .map(|l| &l[1..])
        .filter(|c| !c.trim().is_empty())
        .map(|c| c.len() - c.trim_start().len())
        .min();
    let Some(min) = min else { return diff.to_string() };
    if min == 0 {
        return diff.to_string();
    }
    diff.lines()
        .map(|l| {
            if is_content(l) {
                let (prefix, content) = l.split_at(1);
                format!("{prefix}{}", content.chars().skip(min).collect::<String>())
            } else {
                l.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn unified_diff(path: &str, old: &str, new: &str) -> String {
    let d = similar::TextDiff::from_lines(old, new);
    d.unified_diff().context_radius(3).header(path, path).to_string()
}

pub fn diff_stats(old: &str, new: &str) -> (u64, u64) {
    let d = similar::TextDiff::from_lines(old, new);
    let mut add = 0;
    let mut del = 0;
    for op in d.iter_all_changes() {
        match op.tag() {
            similar::ChangeTag::Insert => add += 1,
            similar::ChangeTag::Delete => del += 1,
            _ => {}
        }
    }
    (add, del)
}

fn write_with_dirs(path: &Path, content: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, content)
}

async fn after_write(
    ctx: &ToolCtx,
    path: &Path,
    diff: &str,
    old: &str,
    new: &str,
    first_line: &str,
) -> ToolResult {
    ctx.engine.bus.publish(Event::FileEdited {
        file: path.display().to_string(),
    });
    let (additions, deletions) = diff_stats(old, new);
    let filediff = json!({ "file": path.display().to_string(), "patch": diff, "additions": additions, "deletions": deletions });
    ctx.report(
        None,
        Some(json!({ "diff": diff, "filediff": filediff, "diagnostics": {} })),
    );
    let mut output = first_line.to_string();
    if let Some(block) = ctx.engine.lsp_diagnostics_after_edit(path).await {
        output.push_str(&format!(
            "\n\nLSP errors detected in this file, please fix:\n{block}"
        ));
    }
    ToolResult {
        title: path
            .strip_prefix(ctx.worktree())
            .unwrap_or(path)
            .display()
            .to_string(),
        output,
        metadata: json!({ "diff": diff, "filediff": filediff, "diagnostics": {} }),
        attachments: Vec::new(),
    }
}

// ───────────────────────────── edit ─────────────────────────────

#[derive(Deserialize)]
struct EditArgs {
    #[serde(rename = "filePath")]
    file_path: String,
    #[serde(rename = "oldString")]
    old_string: String,
    #[serde(rename = "newString")]
    new_string: String,
    #[serde(rename = "replaceAll", default)]
    replace_all: bool,
}

pub struct EditTool;

#[async_trait]
impl Tool for EditTool {
    fn id(&self) -> &'static str {
        "edit"
    }
    fn description(&self) -> Cow<'static, str> {
        Cow::Borrowed(crate::tool_description!("edit"))
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "filePath": { "type": "string", "description": "Absolute path" },
                "oldString": { "type": "string", "description": "Exact text to find" },
                "newString": { "type": "string", "description": "Replacement" },
                "replaceAll": { "type": "boolean", "description": "Replace every match" }
            },
            "required": ["filePath", "oldString", "newString"]
        })
    }

    async fn execute(&self, ctx: ToolCtx, args: Value) -> Result<ToolResult, ToolError> {
        let args: EditArgs = parse_args(args)?;
        if args.file_path.is_empty() {
            return Err(ToolError::Invalid("filePath is required".into()));
        }
        if args.old_string == args.new_string {
            return Err(ToolError::Invalid(replacers::ReplaceError::Identical.to_string()));
        }
        let path = ctx.engine.resolve_path(&args.file_path);
        external_directory::assert(&ctx, &path, false).await?;
        let rel = path
            .strip_prefix(ctx.worktree())
            .unwrap_or(&path)
            .display()
            .to_string();
        let lock = lock_for(&path);
        let _guard = lock.lock().await;

        let (content_old, content_new, diff) = if args.old_string.is_empty() {
            if path.exists() {
                return Err(ToolError::Invalid(replacers::ReplaceError::Empty.to_string()));
            }
            let (bom, text) = split_bom(&args.new_string);
            let diff = trim_diff(&unified_diff(&path.display().to_string(), "", text));
            ctx.ask(
                "edit",
                vec![rel.clone()],
                vec!["*".into()],
                json!({ "filepath": path.display().to_string(), "diff": diff })
                    .as_object()
                    .cloned()
                    .unwrap_or_default(),
            )
            .await?;
            let out = if bom {
                format!("{BOM}{text}")
            } else {
                text.to_string()
            };
            write_with_dirs(&path, &out).map_err(ToolError::other)?;
            let new = ctx
                .engine
                .format_file(&path)
                .await
                .unwrap_or_else(|| text.to_string());
            (String::new(), new, diff)
        } else {
            let meta = std::fs::metadata(&path)
                .map_err(|_| ToolError::Other(format!("File {} not found", path.display())))?;
            if meta.is_dir() {
                return Err(ToolError::Other(format!(
                    "Path is a directory, not a file: {}",
                    path.display()
                )));
            }
            let raw = std::fs::read_to_string(&path).map_err(ToolError::other)?;
            let (had_bom, source) = split_bom(&raw);
            let ending = detect_line_ending(source);
            let old = to_line_ending(&args.old_string, ending);
            let new = to_line_ending(&args.new_string, ending);
            let replaced = replacers::replace(source, &old, &new, args.replace_all)
                .map_err(|e| ToolError::Invalid(e.to_string()))?;
            let (new_bom, next) = split_bom(&replaced);
            let bom = had_bom || new_bom;
            let diff = trim_diff(&unified_diff(
                &path.display().to_string(),
                &normalize_line_endings(source),
                &normalize_line_endings(next),
            ));
            ctx.ask(
                "edit",
                vec![rel.clone()],
                vec!["*".into()],
                json!({ "filepath": path.display().to_string(), "diff": diff })
                    .as_object()
                    .cloned()
                    .unwrap_or_default(),
            )
            .await?;
            let out = if bom {
                format!("{BOM}{next}")
            } else {
                next.to_string()
            };
            write_with_dirs(&path, &out).map_err(ToolError::other)?;
            let formatted = ctx
                .engine
                .format_file(&path)
                .await
                .unwrap_or_else(|| next.to_string());
            let diff = trim_diff(&unified_diff(
                &path.display().to_string(),
                &normalize_line_endings(source),
                &normalize_line_endings(&formatted),
            ));
            (source.to_string(), formatted, diff)
        };
        Ok(after_write(
            &ctx,
            &path,
            &diff,
            &content_old,
            &content_new,
            "Edit applied successfully.",
        )
        .await)
    }
}

// ───────────────────────────── write ─────────────────────────────

#[derive(Deserialize)]
struct WriteArgs {
    #[serde(rename = "filePath")]
    file_path: String,
    content: String,
}

pub struct WriteTool;

#[async_trait]
impl Tool for WriteTool {
    fn id(&self) -> &'static str {
        "write"
    }
    fn description(&self) -> Cow<'static, str> {
        Cow::Borrowed(crate::tool_description!("write"))
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "filePath": { "type": "string", "description": "Absolute path" },
                "content": { "type": "string", "description": "Full file content" }
            },
            "required": ["filePath", "content"]
        })
    }

    async fn execute(&self, ctx: ToolCtx, args: Value) -> Result<ToolResult, ToolError> {
        let args: WriteArgs = parse_args(args)?;
        let path = ctx.engine.resolve_path(&args.file_path);
        external_directory::assert(&ctx, &path, false).await?;
        let rel = path
            .strip_prefix(ctx.worktree())
            .unwrap_or(&path)
            .display()
            .to_string();
        let lock = lock_for(&path);
        let _guard = lock.lock().await;
        let exists = path.exists();
        let raw = if exists {
            std::fs::read_to_string(&path).unwrap_or_default()
        } else {
            String::new()
        };
        let (had_bom, old) = split_bom(&raw);
        let (new_bom, new) = split_bom(&args.content);
        let bom = had_bom || new_bom;
        let diff = trim_diff(&unified_diff(&path.display().to_string(), old, new));
        ctx.ask(
            "edit",
            vec![rel],
            vec!["*".into()],
            json!({ "filepath": path.display().to_string(), "diff": diff })
                .as_object()
                .cloned()
                .unwrap_or_default(),
        )
        .await?;
        let out = if bom {
            format!("{BOM}{new}")
        } else {
            new.to_string()
        };
        write_with_dirs(&path, &out).map_err(ToolError::other)?;
        let formatted = ctx
            .engine
            .format_file(&path)
            .await
            .unwrap_or_else(|| new.to_string());
        let mut result = after_write(&ctx, &path, &diff, old, &formatted, "Wrote file successfully.").await;
        if let Value::Object(m) = &mut result.metadata {
            m.insert("filepath".into(), json!(path.display().to_string()));
            m.insert("exists".into(), json!(exists));
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trims_common_indent_in_diff() {
        let d = "--- a\n+++ b\n@@ -1,2 +1,2 @@\n     foo\n-    bar\n+    baz\n";
        let t = trim_diff(d);
        assert!(t.contains("\n foo\n-bar\n+baz"));
    }

    #[test]
    fn stats() {
        assert_eq!(diff_stats("a\nb\n", "a\nc\nd\n"), (2, 1));
    }
}
