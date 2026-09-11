//! `glob` and `grep` tools built on ripgrep's own crates (`ignore`,
//! `globset`, `grep-regex`, `grep-searcher`) — no external `rg` binary.

use std::borrow::Cow;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use grep_searcher::{Searcher, SearcherBuilder, sinks::UTF8};
use serde::Deserialize;
use serde_json::{Value, json};

use super::external_directory;
use crate::tool::{Tool, ToolCtx, ToolError, ToolResult, parse_args};

const LIMIT: usize = 100;
const MAX_LINE_CHARS: usize = 2000;

fn walker(root: &Path) -> ignore::WalkBuilder {
    let mut b = ignore::WalkBuilder::new(root);
    b.hidden(false)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .follow_links(false);
    b.filter_entry(|e| e.file_name() != ".git");
    b
}

/// Files under `root` matching `pattern` (relative glob), newest mtime first.
pub fn glob_files(root: &Path, pattern: &str, limit: usize) -> Result<(Vec<PathBuf>, bool), String> {
    let matcher = globset::GlobBuilder::new(pattern)
        .literal_separator(false)
        .build()
        .map_err(|e| e.to_string())?
        .compile_matcher();
    let mut found: Vec<(PathBuf, std::time::SystemTime)> = Vec::new();
    for entry in walker(root).build().flatten() {
        let p = entry.path();
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let rel = p.strip_prefix(root).unwrap_or(p);
        if matcher.is_match(rel) || matcher.is_match(p) {
            let mtime = entry
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .unwrap_or(std::time::UNIX_EPOCH);
            found.push((p.to_path_buf(), mtime));
        }
    }
    found.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let truncated = found.len() > limit;
    found.truncate(limit);
    Ok((found.into_iter().map(|(p, _)| p).collect(), truncated))
}

pub struct GrepHit {
    pub path: PathBuf,
    pub line: u64,
    pub text: String,
}

pub fn grep_files(
    root: &Path,
    pattern: &str,
    include: Option<&str>,
    limit: usize,
) -> Result<(Vec<GrepHit>, bool), String> {
    let matcher = grep_regex::RegexMatcherBuilder::new()
        .build(pattern)
        .map_err(|e| e.to_string())?;
    let include = match include {
        Some(g) => Some(
            globset::GlobBuilder::new(g)
                .literal_separator(false)
                .build()
                .map_err(|e| e.to_string())?
                .compile_matcher(),
        ),
        None => None,
    };
    let mut searcher: Searcher = SearcherBuilder::new().line_number(true).build();
    let mut hits: Vec<GrepHit> = Vec::new();
    let mut truncated = false;
    let files: Vec<PathBuf> = if root.is_file() {
        vec![root.to_path_buf()]
    } else {
        walker(root)
            .sort_by_file_path(|a, b| a.cmp(b))
            .build()
            .flatten()
            .filter(|e| e.file_type().is_some_and(|t| t.is_file()))
            .map(|e| e.path().to_path_buf())
            .collect()
    };
    'files: for path in files {
        if let Some(inc) = &include {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            let rel = path.strip_prefix(root).unwrap_or(&path);
            if !(inc.is_match(&name) || inc.is_match(rel)) {
                continue;
            }
        }
        let mut local: Vec<(u64, String)> = Vec::new();
        let result = searcher.search_path(
            &matcher,
            &path,
            UTF8(|line, text| {
                let text = text.trim_end_matches(['\r', '\n']);
                let text: String = if text.chars().count() > MAX_LINE_CHARS {
                    text.chars().take(MAX_LINE_CHARS).collect()
                } else {
                    text.to_string()
                };
                local.push((line, text));
                Ok(true)
            }),
        );
        if result.is_err() {
            continue;
        }
        for (line, text) in local {
            if hits.len() >= limit {
                truncated = true;
                break 'files;
            }
            hits.push(GrepHit {
                path: path.clone(),
                line,
                text,
            });
        }
    }
    Ok((hits, truncated))
}

// ───────────────────────────── glob ─────────────────────────────

#[derive(Deserialize)]
struct GlobArgs {
    pattern: String,
    #[serde(default)]
    path: Option<String>,
}

pub struct GlobTool;

#[async_trait]
impl Tool for GlobTool {
    fn id(&self) -> &'static str {
        "glob"
    }
    fn description(&self) -> Cow<'static, str> {
        Cow::Borrowed(crate::tool_description!("glob"))
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "The glob pattern to match files against" },
                "path": { "type": "string", "description": "The directory to search in. If not specified, the current working directory will be used. IMPORTANT: Omit this field to use the default directory. DO NOT enter \"undefined\" or \"null\" - simply omit it for the default behavior. Must be a valid directory path if provided." }
            },
            "required": ["pattern"]
        })
    }
    async fn execute(&self, ctx: ToolCtx, args: Value) -> Result<ToolResult, ToolError> {
        let args: GlobArgs = parse_args(args)?;
        ctx.ask(
            "glob",
            vec![args.pattern.clone()],
            vec!["*".into()],
            json!({ "pattern": args.pattern, "path": args.path })
                .as_object()
                .cloned()
                .unwrap_or_default(),
        )
        .await?;
        let search = match &args.path {
            Some(p) if !p.is_empty() && p != "undefined" && p != "null" => ctx.engine.resolve_path(p),
            _ => ctx.directory().to_path_buf(),
        };
        if search.is_file() {
            return Err(ToolError::Invalid(format!(
                "glob path must be a directory: {}",
                search.display()
            )));
        }
        external_directory::assert(&ctx, &search, true).await?;
        let (files, truncated) = tokio::task::spawn_blocking({
            let s = search.clone();
            let p = args.pattern.clone();
            move || glob_files(&s, &p, LIMIT)
        })
        .await
        .map_err(ToolError::other)?
        .map_err(ToolError::Invalid)?;
        let mut out: Vec<String> = if files.is_empty() {
            vec!["No files found".into()]
        } else {
            files.iter().map(|f| f.display().to_string()).collect()
        };
        if truncated {
            out.push(String::new());
            out.push(format!("(Results are truncated: showing first {LIMIT} results. Consider using a more specific path or pattern.)"));
        }
        Ok(ToolResult {
            title: search
                .strip_prefix(ctx.worktree())
                .unwrap_or(&search)
                .display()
                .to_string(),
            metadata: json!({ "count": files.len(), "truncated": truncated }),
            output: out.join("\n"),
            attachments: Vec::new(),
        })
    }
}

// ───────────────────────────── grep ─────────────────────────────

#[derive(Deserialize)]
struct GrepArgs {
    pattern: String,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    include: Option<String>,
}

pub struct GrepTool;

#[async_trait]
impl Tool for GrepTool {
    fn id(&self) -> &'static str {
        "grep"
    }
    fn description(&self) -> Cow<'static, str> {
        Cow::Borrowed(crate::tool_description!("grep"))
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "The regex pattern to search for in file contents" },
                "path": { "type": "string", "description": "The directory to search in. Defaults to the current working directory." },
                "include": { "type": "string", "description": "File pattern to include in the search (e.g. \"*.js\", \"*.{ts,tsx}\")" }
            },
            "required": ["pattern"]
        })
    }
    async fn execute(&self, ctx: ToolCtx, args: Value) -> Result<ToolResult, ToolError> {
        let args: GrepArgs = parse_args(args)?;
        if args.pattern.is_empty() {
            return Err(ToolError::Invalid("pattern is required".into()));
        }
        ctx.ask(
            "grep",
            vec![args.pattern.clone()],
            vec!["*".into()],
            json!({ "pattern": args.pattern, "path": args.path, "include": args.include })
                .as_object()
                .cloned()
                .unwrap_or_default(),
        )
        .await?;
        let requested = match &args.path {
            Some(p) if !p.is_empty() => ctx.engine.resolve_path(p),
            _ => ctx.directory().to_path_buf(),
        };
        external_directory::assert(&ctx, &requested, requested.is_dir()).await?;
        let (hits, truncated) = tokio::task::spawn_blocking({
            let r = requested.clone();
            let p = args.pattern.clone();
            let inc = args.include.clone();
            move || grep_files(&r, &p, inc.as_deref(), LIMIT)
        })
        .await
        .map_err(ToolError::other)?
        .map_err(ToolError::Invalid)?;
        if hits.is_empty() {
            return Ok(ToolResult {
                title: args.pattern.clone(),
                metadata: json!({ "matches": 0, "truncated": false }),
                output: "No files found".into(),
                attachments: Vec::new(),
            });
        }
        let mut out = vec![format!(
            "Found {} matches{}",
            hits.len(),
            if truncated {
                " (more matches available)"
            } else {
                ""
            }
        )];
        let mut current = PathBuf::new();
        for h in &hits {
            if current != h.path {
                if !current.as_os_str().is_empty() {
                    out.push(String::new());
                }
                current = h.path.clone();
                out.push(format!("{}:", h.path.display()));
            }
            out.push(format!("  Line {}: {}", h.line, h.text));
        }
        if truncated {
            out.push(String::new());
            out.push("(Results truncated. Consider using a more specific path or pattern.)".into());
        }
        Ok(ToolResult {
            title: args.pattern.clone(),
            metadata: json!({ "matches": hits.len(), "truncated": truncated }),
            output: out.join("\n"),
            attachments: Vec::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_and_grep() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src/a")).unwrap();
        std::fs::write(dir.path().join("src/a/x.rs"), "fn main() {}\nlet needle = 1;\n").unwrap();
        std::fs::write(dir.path().join("src/y.txt"), "needle here\n").unwrap();
        let (files, _) = glob_files(dir.path(), "**/*.rs", 10).unwrap();
        assert_eq!(files.len(), 1);
        let (hits, trunc) = grep_files(dir.path(), "needle", None, 10).unwrap();
        assert_eq!(hits.len(), 2);
        assert!(!trunc);
        let (hits, _) = grep_files(dir.path(), "needle", Some("*.rs"), 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].line, 2);
    }
}
