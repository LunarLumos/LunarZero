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

/// Outcome of applying only some hunks of a proposed change.
pub struct Partial {
    /// The content actually written.
    pub content: String,
    /// Diff text of the hunks the user left out (one `@@` block each).
    pub rejected: Vec<String>,
    pub total: usize,
}

/// Apply only the selected hunks (0-based, numbered like the `@@` sections
/// of `unified_diff`) of the `old → new` change; other hunks keep `old`.
pub fn apply_hunks(old: &str, new: &str, selected: &[usize]) -> Partial {
    use similar::{DiffTag, TextDiff};
    let diff = TextDiff::from_lines(old, new);
    let groups = diff.grouped_ops(3);
    let total = groups.len();
    // which group each change op belongs to, keyed by its old-side start
    let mut group_of = std::collections::HashMap::new();
    let mut rejected = Vec::new();
    for (gi, group) in groups.iter().enumerate() {
        for op in group {
            if op.tag() != DiffTag::Equal {
                group_of.insert((op.old_range().start, op.new_range().start), gi);
            }
        }
        if !selected.contains(&gi) {
            // render the hunk the way the user saw it
            let hunk = diff
                .unified_diff()
                .context_radius(3)
                .iter_hunks()
                .nth(gi)
                .map(|h| h.to_string())
                .unwrap_or_default();
            rejected.push(hunk);
        }
    }
    let old_lines: Vec<&str> = diff.old_slices().to_vec();
    let new_lines: Vec<&str> = diff.new_slices().to_vec();
    let mut content = String::new();
    for op in diff.ops() {
        let take_new = match op.tag() {
            DiffTag::Equal => false,
            _ => group_of
                .get(&(op.old_range().start, op.new_range().start))
                .is_some_and(|gi| selected.contains(gi)),
        };
        if op.tag() == DiffTag::Equal || !take_new {
            for l in &old_lines[op.old_range()] {
                content.push_str(l);
            }
        } else {
            for l in &new_lines[op.new_range()] {
                content.push_str(l);
            }
        }
    }
    Partial {
        content,
        rejected,
        total,
    }
}

/// Note for the model when the user applied only part of a change.
fn partial_note(p: &Partial, note: Option<&str>) -> String {
    let mut out = format!(
        "\n\nThe user applied {} of {} hunks. These hunks were NOT applied (the file keeps its previous content there):\n",
        p.total - p.rejected.len(),
        p.total
    );
    for (i, h) in p.rejected.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        out.push_str(h.trim_end());
        out.push('\n');
    }
    match note {
        Some(n) => out.push_str(&format!("User's note about them: {n}\nRe-read the file before editing it again.")),
        None => out.push_str("Treat them as rejected unless the user asks otherwise; re-read the file before editing it again."),
    }
    out
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
    let mut diagnostics = json!({});
    if let Some(block) = ctx.engine.lsp_diagnostics_after_edit(path).await {
        let errors = block.matches("ERROR [").count();
        output.push_str(&format!(
            "\n\nThe language server reports {errors} error(s) in this file — fix them before running builds or tests:\n{block}"
        ));
        diagnostics = json!({ "errors": errors, "text": block });
    }
    ToolResult {
        title: path
            .strip_prefix(ctx.worktree())
            .unwrap_or(path)
            .display()
            .to_string(),
        output,
        metadata: json!({ "diff": diff, "filediff": filediff, "diagnostics": diagnostics }),
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

        let mut partial: Option<String> = None;
        let (content_old, content_new, diff) = if args.old_string.is_empty() {
            if path.exists() {
                return Err(ToolError::Invalid(replacers::ReplaceError::Empty.to_string()));
            }
            let (bom, text) = split_bom(&args.new_string);
            let diff = trim_diff(&unified_diff(&path.display().to_string(), "", text));
            let grant = ctx
                .ask(
                    "edit",
                    vec![rel.clone()],
                    vec!["*".into()],
                    json!({ "filepath": path.display().to_string(), "diff": diff })
                        .as_object()
                        .cloned()
                        .unwrap_or_default(),
                )
                .await?;
            if grant.hunks.as_ref().is_some_and(|h| h.is_empty()) {
                return Err(ToolError::Other(format!(
                    "The user did not apply this change.{}",
                    grant.note.map(|n| format!(" Note: {n}")).unwrap_or_default()
                )));
            }
            let out = if bom {
                format!("{BOM}{text}")
            } else {
                text.to_string()
            };
            write_with_dirs(&path, &out).map_err(ToolError::other)?;
            let new = ctx
                .engine
                .format_file(&path, None)
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
            let grant = ctx
                .ask(
                    "edit",
                    vec![rel.clone()],
                    vec!["*".into()],
                    json!({ "filepath": path.display().to_string(), "diff": diff })
                        .as_object()
                        .cloned()
                        .unwrap_or_default(),
                )
                .await?;
            let (next, note) = match &grant.hunks {
                Some(sel) => {
                    let p = apply_hunks(source, next, sel);
                    if p.rejected.len() == p.total {
                        return Err(ToolError::Other(format!(
                            "The user did not apply this change.{}",
                            grant
                                .note
                                .as_ref()
                                .map(|n| format!(" Note: {n}"))
                                .unwrap_or_default()
                        )));
                    }
                    let note = (!p.rejected.is_empty()).then(|| partial_note(&p, grant.note.as_deref()));
                    (p.content, note)
                }
                None => (next.to_string(), None),
            };
            partial = note;
            let out = if bom {
                format!("{BOM}{next}")
            } else {
                next.to_string()
            };
            write_with_dirs(&path, &out).map_err(ToolError::other)?;
            let formatted = ctx
                .engine
                .format_file(&path, Some(source))
                .await
                .unwrap_or_else(|| next.to_string());
            let diff = trim_diff(&unified_diff(
                &path.display().to_string(),
                &normalize_line_endings(source),
                &normalize_line_endings(&formatted),
            ));
            (source.to_string(), formatted, diff)
        };
        let mut result = after_write(
            &ctx,
            &path,
            &diff,
            &content_old,
            &content_new,
            "Edit applied successfully.",
        )
        .await;
        if let Some(note) = partial {
            result.output.push_str(&note);
        }
        Ok(result)
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
        let grant = ctx
            .ask(
                "edit",
                vec![rel],
                vec!["*".into()],
                json!({ "filepath": path.display().to_string(), "diff": diff })
                    .as_object()
                    .cloned()
                    .unwrap_or_default(),
            )
            .await?;
        let (new, partial) = match &grant.hunks {
            Some(sel) => {
                let p = apply_hunks(old, new, sel);
                if p.rejected.len() == p.total {
                    return Err(ToolError::Other(format!(
                        "The user did not apply this change.{}",
                        grant
                            .note
                            .as_ref()
                            .map(|n| format!(" Note: {n}"))
                            .unwrap_or_default()
                    )));
                }
                let note = (!p.rejected.is_empty()).then(|| partial_note(&p, grant.note.as_deref()));
                (p.content, note)
            }
            None => (new.to_string(), None),
        };
        let new = new.as_str();
        let out = if bom {
            format!("{BOM}{new}")
        } else {
            new.to_string()
        };
        write_with_dirs(&path, &out).map_err(ToolError::other)?;
        let formatted = ctx
            .engine
            .format_file(&path, exists.then_some(old))
            .await
            .unwrap_or_else(|| new.to_string());
        let mut result = after_write(&ctx, &path, &diff, old, &formatted, "Wrote file successfully.").await;
        if let Some(note) = partial {
            result.output.push_str(&note);
        }
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
    fn partial_hunks() {
        let old = "a\nb\nc\nd\ne\nf\ng\nh\ni\nj\nk\nl\nm\nn\n";
        let new = "a\nB\nc\nd\ne\nf\ng\nh\ni\nj\nk\nl\nM\nn\n";
        let full = unified_diff("f", old, new);
        assert_eq!(full.matches("@@").count() / 2, 2, "{full}");
        let p = apply_hunks(old, new, &[0]);
        assert_eq!(p.total, 2);
        assert_eq!(p.rejected.len(), 1);
        assert_eq!(p.content, "a\nB\nc\nd\ne\nf\ng\nh\ni\nj\nk\nl\nm\nn\n");
        assert!(p.rejected[0].contains("-m\n+M"), "{}", p.rejected[0]);
        let p = apply_hunks(old, new, &[1]);
        assert_eq!(p.content, "a\nb\nc\nd\ne\nf\ng\nh\ni\nj\nk\nl\nM\nn\n");
        let p = apply_hunks(old, new, &[0, 1]);
        assert_eq!(p.content, new);
        let p = apply_hunks(old, new, &[]);
        assert_eq!(p.content, old);
    }

    #[test]
    fn stats() {
        assert_eq!(diff_stats("a\nb\n", "a\nc\nd\n"), (2, 1));
    }
}
