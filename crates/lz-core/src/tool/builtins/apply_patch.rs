//! `apply_patch` — OpenAI Codex patch format (`*** Begin Patch` … `*** End Patch`).
//! Used instead of edit/write for gpt-* models.

use std::borrow::Cow;
use std::path::PathBuf;

use async_trait::async_trait;
use lz_schema::Event;
use serde::Deserialize;
use serde_json::{Value, json};

use super::edit::{diff_stats, trim_diff, unified_diff};
use super::external_directory;
use crate::tool::{Tool, ToolCtx, ToolError, ToolResult, parse_args};

#[derive(Debug, Clone, PartialEq)]
pub struct Chunk {
    pub old_lines: Vec<String>,
    pub new_lines: Vec<String>,
    pub change_context: Option<String>,
    pub is_end_of_file: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Hunk {
    Add {
        path: String,
        contents: String,
    },
    Delete {
        path: String,
    },
    Update {
        path: String,
        move_path: Option<String>,
        chunks: Vec<Chunk>,
    },
}

fn strip_heredoc(input: &str) -> &str {
    let re = regex::Regex::new(r"(?s)^(?:cat\s+)?<<['\x22]?(\w+)['\x22]?\s*\n(.*?)\n(\w+)\s*$").ok();
    if let Some(re) = re
        && let Some(c) = re.captures(input)
        && c.get(1).map(|m| m.as_str()) == c.get(3).map(|m| m.as_str())
    {
        return c.get(2).map(|m| m.as_str()).unwrap_or(input);
    }
    input
}

pub fn parse(patch: &str) -> Result<Vec<Hunk>, String> {
    let cleaned = strip_heredoc(patch.trim());
    let lines: Vec<&str> = cleaned.split('\n').collect();
    let begin = lines.iter().position(|l| l.trim() == "*** Begin Patch");
    let end = lines.iter().position(|l| l.trim() == "*** End Patch");
    let (Some(b), Some(e)) = (begin, end) else {
        return Err("Invalid patch format: missing Begin/End markers".into());
    };
    if b >= e {
        return Err("Invalid patch format: missing Begin/End markers".into());
    }
    let mut hunks = Vec::new();
    let mut i = b + 1;
    while i < e {
        let line = lines[i];
        if let Some(p) = line.strip_prefix("*** Add File:") {
            let path = p.trim().to_string();
            i += 1;
            let mut content = String::new();
            while i < lines.len() && !lines[i].starts_with("***") {
                if let Some(c) = lines[i].strip_prefix('+') {
                    content.push_str(c);
                    content.push('\n');
                }
                i += 1;
            }
            if content.ends_with('\n') {
                content.pop();
            }
            if !path.is_empty() {
                hunks.push(Hunk::Add {
                    path,
                    contents: content,
                });
            }
        } else if let Some(p) = line.strip_prefix("*** Delete File:") {
            let path = p.trim().to_string();
            i += 1;
            if !path.is_empty() {
                hunks.push(Hunk::Delete { path });
            }
        } else if let Some(p) = line.strip_prefix("*** Update File:") {
            let path = p.trim().to_string();
            i += 1;
            let mut move_path = None;
            if i < lines.len()
                && let Some(m) = lines[i].strip_prefix("*** Move to:")
            {
                move_path = Some(m.trim().to_string());
                i += 1;
            }
            let mut chunks = Vec::new();
            while i < lines.len() && !lines[i].starts_with("***") {
                if let Some(ctx) = lines[i].strip_prefix("@@") {
                    let context = ctx.trim().to_string();
                    i += 1;
                    let mut old = Vec::new();
                    let mut new = Vec::new();
                    let mut eof = false;
                    while i < lines.len() && !lines[i].starts_with("@@") && !lines[i].starts_with("***") {
                        let l = lines[i];
                        if l == "*** End of File" {
                            eof = true;
                            i += 1;
                            break;
                        }
                        if let Some(c) = l.strip_prefix(' ') {
                            old.push(c.to_string());
                            new.push(c.to_string());
                        } else if let Some(c) = l.strip_prefix('-') {
                            old.push(c.to_string());
                        } else if let Some(c) = l.strip_prefix('+') {
                            new.push(c.to_string());
                        }
                        i += 1;
                    }
                    chunks.push(Chunk {
                        old_lines: old,
                        new_lines: new,
                        change_context: if context.is_empty() { None } else { Some(context) },
                        is_end_of_file: eof,
                    });
                } else {
                    i += 1;
                }
            }
            if !path.is_empty() {
                hunks.push(Hunk::Update {
                    path,
                    move_path,
                    chunks,
                });
            }
        } else {
            i += 1;
        }
    }
    Ok(hunks)
}

fn normalize_unicode(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}' => '\'',
            '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{201F}' => '"',
            '\u{2010}' | '\u{2011}' | '\u{2012}' | '\u{2013}' | '\u{2014}' | '\u{2015}' => '-',
            '\u{00A0}' => ' ',
            other => other,
        })
        .collect::<String>()
        .replace('\u{2026}', "...")
}

fn try_match(
    lines: &[String],
    pattern: &[String],
    start: usize,
    cmp: &dyn Fn(&str, &str) -> bool,
    eof: bool,
) -> Option<usize> {
    if pattern.len() > lines.len() {
        return None;
    }
    if eof {
        let from_end = lines.len() - pattern.len();
        if from_end >= start && (0..pattern.len()).all(|j| cmp(&lines[from_end + j], &pattern[j])) {
            return Some(from_end);
        }
    }
    (start..=lines.len() - pattern.len())
        .find(|&i| (0..pattern.len()).all(|j| cmp(&lines[i + j], &pattern[j])))
}

fn seek(lines: &[String], pattern: &[String], start: usize, eof: bool) -> Option<usize> {
    if pattern.is_empty() {
        return None;
    }
    try_match(lines, pattern, start, &|a, b| a == b, eof)
        .or_else(|| try_match(lines, pattern, start, &|a, b| a.trim_end() == b.trim_end(), eof))
        .or_else(|| try_match(lines, pattern, start, &|a, b| a.trim() == b.trim(), eof))
        .or_else(|| {
            try_match(
                lines,
                pattern,
                start,
                &|a, b| normalize_unicode(a.trim()) == normalize_unicode(b.trim()),
                eof,
            )
        })
}

/// Apply update chunks to file text → new text.
pub fn apply_chunks(path: &str, chunks: &[Chunk], original: &str) -> Result<String, String> {
    let mut lines: Vec<String> = original.split('\n').map(str::to_string).collect();
    if lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    let mut replacements: Vec<(usize, usize, Vec<String>)> = Vec::new();
    let mut idx = 0usize;
    for chunk in chunks {
        if let Some(ctx) = &chunk.change_context {
            let found = seek(&lines, std::slice::from_ref(ctx), idx, false)
                .ok_or_else(|| format!("Failed to find context '{ctx}' in {path}"))?;
            idx = found + 1;
        }
        if chunk.old_lines.is_empty() {
            let insert = if lines.last().is_some_and(|l| l.is_empty()) {
                lines.len() - 1
            } else {
                lines.len()
            };
            replacements.push((insert, 0, chunk.new_lines.clone()));
            continue;
        }
        let mut pattern = chunk.old_lines.clone();
        let mut new_slice = chunk.new_lines.clone();
        let mut found = seek(&lines, &pattern, idx, chunk.is_end_of_file);
        if found.is_none() && pattern.last().is_some_and(|l| l.is_empty()) {
            pattern.pop();
            if new_slice.last().is_some_and(|l| l.is_empty()) {
                new_slice.pop();
            }
            found = seek(&lines, &pattern, idx, chunk.is_end_of_file);
        }
        match found {
            Some(f) => {
                replacements.push((f, pattern.len(), new_slice));
                idx = f + pattern.len();
            }
            None => {
                return Err(format!(
                    "Failed to find expected lines in {path}:\n{}",
                    chunk.old_lines.join("\n")
                ));
            }
        }
    }
    replacements.sort_by_key(|r| r.0);
    for (start, old_len, new) in replacements.into_iter().rev() {
        let end = (start + old_len).min(lines.len());
        lines.splice(start..end, new);
    }
    if lines.last().is_none_or(|l| !l.is_empty()) {
        lines.push(String::new());
    }
    Ok(lines.join("\n"))
}

#[derive(Deserialize)]
struct Args {
    #[serde(rename = "patchText")]
    patch_text: String,
}

struct Change {
    path: PathBuf,
    new: String,
    kind: &'static str,
    move_path: Option<PathBuf>,
    diff: String,
    additions: u64,
    deletions: u64,
}

pub struct ApplyPatchTool;

#[async_trait]
impl Tool for ApplyPatchTool {
    fn id(&self) -> &'static str {
        "apply_patch"
    }
    fn description(&self) -> Cow<'static, str> {
        Cow::Borrowed(crate::tool_description!("apply_patch"))
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": { "patchText": { "type": "string", "description": "Patch text" } },
            "required": ["patchText"]
        })
    }
    async fn execute(&self, ctx: ToolCtx, args: Value) -> Result<ToolResult, ToolError> {
        let args: Args = parse_args(args)?;
        let hunks = parse(&args.patch_text)
            .map_err(|e| ToolError::Invalid(format!("apply_patch verification failed: {e}")))?;
        if hunks.is_empty() {
            let norm = args.patch_text.replace("\r\n", "\n").replace('\r', "\n");
            if norm.trim() == "*** Begin Patch\n*** End Patch" {
                return Err(ToolError::Invalid("patch rejected: empty patch".into()));
            }
            return Err(ToolError::Invalid(
                "apply_patch verification failed: no hunks found".into(),
            ));
        }
        let mut changes: Vec<Change> = Vec::new();
        let mut total_diff = String::new();
        for hunk in &hunks {
            match hunk {
                Hunk::Add { path, contents } => {
                    let p = ctx.engine.resolve_path(path);
                    external_directory::assert(&ctx, &p, false).await?;
                    let new = if contents.is_empty() || contents.ends_with('\n') {
                        contents.clone()
                    } else {
                        format!("{contents}\n")
                    };
                    let diff = trim_diff(&unified_diff(&p.display().to_string(), "", &new));
                    let (a, d) = diff_stats("", &new);
                    total_diff.push_str(&diff);
                    total_diff.push('\n');
                    changes.push(Change {
                        path: p,
                        new,
                        kind: "add",
                        move_path: None,
                        diff,
                        additions: a,
                        deletions: d,
                    });
                }
                Hunk::Update {
                    path,
                    move_path,
                    chunks,
                } => {
                    let p = ctx.engine.resolve_path(path);
                    external_directory::assert(&ctx, &p, false).await?;
                    let old = std::fs::read_to_string(&p).map_err(|_| {
                        ToolError::Invalid(format!(
                            "apply_patch verification failed: Failed to read file to update: {}",
                            p.display()
                        ))
                    })?;
                    let new = apply_chunks(&p.display().to_string(), chunks, &old)
                        .map_err(|e| ToolError::Invalid(format!("apply_patch verification failed: {e}")))?;
                    let diff = trim_diff(&unified_diff(&p.display().to_string(), &old, &new));
                    let (a, d) = diff_stats(&old, &new);
                    let mv = move_path.as_ref().map(|m| ctx.engine.resolve_path(m));
                    if let Some(m) = &mv {
                        external_directory::assert(&ctx, m, false).await?;
                    }
                    total_diff.push_str(&diff);
                    total_diff.push('\n');
                    changes.push(Change {
                        path: p,
                        new,
                        kind: if mv.is_some() { "move" } else { "update" },
                        move_path: mv,
                        diff,
                        additions: a,
                        deletions: d,
                    });
                }
                Hunk::Delete { path } => {
                    let p = ctx.engine.resolve_path(path);
                    external_directory::assert(&ctx, &p, false).await?;
                    let old = std::fs::read_to_string(&p)
                        .map_err(|e| ToolError::Invalid(format!("apply_patch verification failed: {e}")))?;
                    let diff = trim_diff(&unified_diff(&p.display().to_string(), &old, ""));
                    let deletions = old.split('\n').count() as u64;
                    total_diff.push_str(&diff);
                    total_diff.push('\n');
                    changes.push(Change {
                        path: p,
                        new: String::new(),
                        kind: "delete",
                        move_path: None,
                        diff,
                        additions: 0,
                        deletions,
                    });
                }
            }
        }
        let rel = |p: &PathBuf| {
            p.strip_prefix(ctx.worktree())
                .unwrap_or(p)
                .display()
                .to_string()
                .replace('\\', "/")
        };
        let files: Vec<Value> = changes
            .iter()
            .map(|c| {
                json!({
                    "filePath": c.path.display().to_string(),
                    "relativePath": rel(c.move_path.as_ref().unwrap_or(&c.path)),
                    "type": c.kind, "patch": c.diff, "additions": c.additions, "deletions": c.deletions,
                    "movePath": c.move_path.as_ref().map(|m| m.display().to_string())
                })
            })
            .collect();
        let rels: Vec<String> = changes.iter().map(|c| rel(&c.path)).collect();
        ctx.ask(
            "edit",
            rels.clone(),
            vec!["*".into()],
            json!({ "filepath": rels.join(", "), "diff": total_diff, "files": files })
                .as_object()
                .cloned()
                .unwrap_or_default(),
        )
        .await?;

        let mut summary = Vec::new();
        for c in &changes {
            match c.kind {
                "add" | "update" => {
                    if let Some(parent) = c.path.parent() {
                        let _ = std::fs::create_dir_all(parent);
                    }
                    std::fs::write(&c.path, &c.new).map_err(ToolError::other)?;
                }
                "move" => {
                    let m = c.move_path.as_ref().unwrap();
                    if let Some(parent) = m.parent() {
                        let _ = std::fs::create_dir_all(parent);
                    }
                    std::fs::write(m, &c.new).map_err(ToolError::other)?;
                    let _ = std::fs::remove_file(&c.path);
                }
                _ => {
                    std::fs::remove_file(&c.path).map_err(ToolError::other)?;
                }
            }
            let target = c.move_path.clone().unwrap_or_else(|| c.path.clone());
            if c.kind != "delete" {
                let _ = ctx.engine.format_file(&target, None).await;
                ctx.engine.bus.publish(Event::FileEdited {
                    file: target.display().to_string(),
                });
            }
            summary.push(match c.kind {
                "add" => format!("A {}", rel(&c.path)),
                "delete" => format!("D {}", rel(&c.path)),
                _ => format!("M {}", rel(&target)),
            });
        }
        let mut output = format!("Success. Updated the following files:\n{}", summary.join("\n"));
        for c in &changes {
            if c.kind == "delete" {
                continue;
            }
            let target = c.move_path.clone().unwrap_or_else(|| c.path.clone());
            if let Some(block) = ctx.engine.lsp_diagnostics_after_edit(&target).await {
                output.push_str(&format!(
                    "\n\nLSP errors detected in {}, please fix:\n{block}",
                    rel(&target)
                ));
            }
        }
        Ok(ToolResult {
            title: output.clone(),
            metadata: json!({ "diff": total_diff, "files": files, "diagnostics": {} }),
            output,
            attachments: Vec::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_applies() {
        let patch = "*** Begin Patch\n*** Update File: a.txt\n@@ fn main\n line1\n-old\n+new\n line3\n*** Add File: b.txt\n+hello\n+world\n*** Delete File: c.txt\n*** End Patch";
        let hunks = parse(patch).unwrap();
        assert_eq!(hunks.len(), 3);
        let Hunk::Update { chunks, .. } = &hunks[0] else {
            panic!()
        };
        assert_eq!(chunks[0].change_context.as_deref(), Some("fn main"));
        let out = apply_chunks("a.txt", chunks, "fn main\nline1\nold\nline3\n").unwrap();
        assert_eq!(out, "fn main\nline1\nnew\nline3\n");
        assert_eq!(
            hunks[1],
            Hunk::Add {
                path: "b.txt".into(),
                contents: "hello\nworld".into()
            }
        );
    }

    #[test]
    fn heredoc_and_missing_lines() {
        let patch =
            "cat <<'EOF'\n*** Begin Patch\n*** Update File: a.txt\n@@\n-zzz\n+yyy\n*** End Patch\nEOF";
        let hunks = parse(patch).unwrap();
        let Hunk::Update { chunks, .. } = &hunks[0] else {
            panic!()
        };
        assert!(apply_chunks("a.txt", chunks, "abc\n").is_err());
        assert!(parse("no markers").is_err());
    }
}
