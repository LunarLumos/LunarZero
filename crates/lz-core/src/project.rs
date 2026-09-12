//! Project identity: git worktree discovery and a stable project ID
//! (sha1 of the normalized remote URL, else a
//! cached id in the git common dir, else the root commit, else `global`).

use std::path::{Path, PathBuf};
use std::process::Command;

use sha1::{Digest, Sha1};

pub const GLOBAL_ID: &str = "global";

#[derive(Debug, Clone)]
pub struct Project {
    pub id: String,
    /// Git worktree root (or the filesystem root when not a git repo).
    pub worktree: PathBuf,
    pub vcs: Option<&'static str>,
    pub git_common_dir: Option<PathBuf>,
}

fn git(args: &[&str], cwd: &Path) -> Option<String> {
    let out = Command::new("git").args(args).current_dir(cwd).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() { None } else { Some(s) }
}

/// `git@github.com:org/repo.git` / `https://github.com/org/repo` → `github.com/org/repo`
pub fn normalize_remote(input: &str) -> Option<String> {
    let value = input.trim();
    if value.is_empty() || value.starts_with("file:") {
        return None;
    }
    let (host, path) = if let Some(idx) = value.find("://") {
        let rest = &value[idx + 3..];
        let rest = rest.rsplit_once('@').map(|(_, r)| r).unwrap_or(rest);
        let (host, path) = rest.split_once('/')?;
        (host.split(':').next().unwrap_or(host), path)
    } else {
        // scp-like: [user@]host:path
        let (head, path) = value.split_once(':')?;
        if head.contains('/') {
            return None;
        }
        let host = head.rsplit_once('@').map(|(_, h)| h).unwrap_or(head);
        (host, path)
    };
    let path = path.trim_start_matches('/').trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path).trim_end_matches('/');
    if host.is_empty() || path.is_empty() {
        return None;
    }
    Some(format!("{}/{}", host.to_lowercase(), path))
}

fn sha1_hex(s: &str) -> String {
    let mut h = Sha1::new();
    h.update(s.as_bytes());
    format!("{:x}", h.finalize())
}

pub fn resolve(directory: &Path) -> Project {
    let Some(worktree) = git(&["rev-parse", "--show-toplevel"], directory) else {
        // not a repository: the directory itself is the boundary — the index,
        // the project map and the external-directory guard all key off it
        return Project {
            id: GLOBAL_ID.into(),
            worktree: directory.to_path_buf(),
            vcs: None,
            git_common_dir: None,
        };
    };
    let worktree = PathBuf::from(worktree);
    let common = git(&["rev-parse", "--git-common-dir"], &worktree).map(|c| {
        let p = PathBuf::from(c);
        if p.is_absolute() { p } else { worktree.join(p) }
    });

    let from_remote = git(&["remote", "get-url", "origin"], &worktree)
        .and_then(|u| normalize_remote(&u))
        .map(|n| sha1_hex(&format!("git-remote:{n}")));
    let cached = common
        .as_ref()
        .and_then(|c| std::fs::read_to_string(c.join("opencode")).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let from_root = || {
        git(&["rev-list", "--max-parents=0", "HEAD"], &worktree)
            .and_then(|s| s.lines().last().map(str::to_string))
    };
    let cached = cached.filter(|c| c != GLOBAL_ID);
    let id = from_remote
        .or(cached)
        .or_else(from_root)
        .unwrap_or_else(|| GLOBAL_ID.into());
    if let Some(c) = &common
        && id != GLOBAL_ID
    {
        let _ = std::fs::write(c.join("opencode"), &id);
    }
    Project {
        id,
        worktree,
        vcs: Some("git"),
        git_common_dir: common,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_remotes() {
        assert_eq!(
            normalize_remote("git@github.com:Org/Repo.git").as_deref(),
            Some("github.com/Org/Repo")
        );
        assert_eq!(
            normalize_remote("https://GitHub.com/org/repo/").as_deref(),
            Some("github.com/org/repo")
        );
        assert_eq!(
            normalize_remote("ssh://git@host:2222/a/b.git").as_deref(),
            Some("host/a/b")
        );
        assert_eq!(normalize_remote("file:///x"), None);
    }
}
