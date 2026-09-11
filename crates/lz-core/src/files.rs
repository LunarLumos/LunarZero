//! File index helpers for the UI: fuzzy file finder and git status.

use std::path::Path;

use lz_schema::api::FileStatus;
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher};

/// Fuzzy-match project files (respecting .gitignore) against `query`.
pub fn find(root: &Path, query: &str, limit: usize) -> Vec<String> {
    let mut files: Vec<String> = Vec::new();
    let walker = ignore::WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        .follow_links(false)
        .build();
    for e in walker.flatten() {
        if files.len() >= 100_000 {
            break;
        }
        if e.file_type().is_some_and(|t| t.is_file())
            && let Ok(rel) = e.path().strip_prefix(root)
        {
            files.push(rel.display().to_string());
        }
    }
    if query.trim().is_empty() {
        files.sort();
        files.truncate(limit);
        return files;
    }
    let mut matcher = Matcher::new(Config::DEFAULT.match_paths());
    let pattern = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);
    let mut scored: Vec<(u32, String)> = files
        .into_iter()
        .filter_map(|f| {
            let mut buf = Vec::new();
            let hay = nucleo_matcher::Utf32Str::new(&f, &mut buf);
            pattern.score(hay, &mut matcher).map(|s| (s, f))
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.len().cmp(&b.1.len())));
    scored.into_iter().take(limit).map(|(_, f)| f).collect()
}

/// `git status --porcelain` + numstat as `FileStatus` rows.
pub async fn git_status(worktree: &Path) -> Vec<FileStatus> {
    let out = tokio::process::Command::new("git")
        .args(["status", "--porcelain=v1", "--untracked-files=all"])
        .current_dir(worktree)
        .output()
        .await;
    let Ok(out) = out else { return Vec::new() };
    if !out.status.success() {
        return Vec::new();
    }
    let mut rows = Vec::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        if line.len() < 4 {
            continue;
        }
        let (code, path) = line.split_at(3);
        let status = match code.trim() {
            "??" | "A" | "AM" => "added",
            "D" | " D" => "deleted",
            _ => "modified",
        };
        rows.push(FileStatus {
            path: path.trim().to_string(),
            status: status.into(),
            additions: 0,
            deletions: 0,
        });
    }
    // line stats for tracked changes
    if let Ok(num) = tokio::process::Command::new("git")
        .args(["diff", "--numstat", "HEAD"])
        .current_dir(worktree)
        .output()
        .await
    {
        for line in String::from_utf8_lossy(&num.stdout).lines() {
            let mut cols = line.split('\t');
            let (Some(a), Some(d), Some(f)) = (cols.next(), cols.next(), cols.next()) else {
                continue;
            };
            if let Some(row) = rows.iter_mut().find(|r| r.path == f) {
                row.additions = a.parse().unwrap_or(0);
                row.deletions = d.parse().unwrap_or(0);
            }
        }
    }
    rows
}
