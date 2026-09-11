//! Install skills from git repositories (GitHub links, `owner/repo`, plain
//! git URLs, optionally pointing at a sub-directory) into the global or
//! project skills directory, where discovery picks them up.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::markdown;
use crate::paths::Paths;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    /// clonable URL
    pub url: String,
    /// branch / tag / commit
    pub git_ref: Option<String>,
    /// directory inside the repo
    pub subpath: Option<String>,
}

impl Source {
    /// Accepts `owner/repo`, `github.com/owner/repo[/tree/<ref>/<path>]`,
    /// `https://…/repo.git`, `git@host:owner/repo.git`, with an optional
    /// `#ref` or `@ref` suffix.
    pub fn parse(input: &str) -> Result<Source, String> {
        let mut s = input.trim().trim_end_matches('/').to_string();
        let mut git_ref = None;
        if let Some((base, r)) = s.rsplit_once('#') {
            git_ref = Some(r.to_string());
            s = base.to_string();
        }
        if s.starts_with("git@") || s.ends_with(".git") && !s.contains("://") {
            return Ok(Source {
                url: s,
                git_ref,
                subpath: None,
            });
        }
        let without_scheme = s
            .strip_prefix("https://")
            .or_else(|| s.strip_prefix("http://"))
            .unwrap_or(&s)
            .to_string();
        let parts: Vec<&str> = without_scheme.split('/').filter(|p| !p.is_empty()).collect();
        // github.com/owner/repo/tree/<ref>/<path…>  |  github.com/owner/repo  |  owner/repo
        let (host, rest): (&str, &[&str]) = if parts.first().is_some_and(|h| h.contains('.')) {
            (parts[0], &parts[1..])
        } else {
            ("github.com", &parts[..])
        };
        if rest.len() < 2 {
            return Err(format!(
                "cannot understand skill source `{input}` — use owner/repo or a GitHub URL"
            ));
        }
        let owner = rest[0];
        let repo = rest[1].trim_end_matches(".git");
        let mut subpath = None;
        if rest.len() > 3 && matches!(rest[2], "tree" | "blob") {
            git_ref = Some(rest[3].to_string());
            if rest.len() > 4 {
                let mut p = rest[4..].to_vec();
                if p.last().is_some_and(|f| *f == "SKILL.md") {
                    p.pop();
                }
                if !p.is_empty() {
                    subpath = Some(p.join("/"));
                }
            }
        } else if rest.len() > 2 {
            subpath = Some(rest[2..].join("/"));
        }
        Ok(Source {
            url: format!("https://{host}/{owner}/{repo}.git"),
            git_ref,
            subpath,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstalledMeta {
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subpath: Option<String>,
    pub installed_at: u64,
}

pub const META_FILE: &str = ".lz-skill.json";

#[derive(Debug, Clone, Serialize)]
pub struct Installed {
    pub name: String,
    pub description: String,
    pub path: PathBuf,
}

/// Where installed skills live: global config dir or the project's `.lunarzero`.
pub fn target_dir(paths: &Paths, worktree: &Path, global: bool) -> PathBuf {
    if global {
        paths.config.join("skills")
    } else {
        worktree.join(".lunarzero").join("skills")
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let name = entry.file_name();
        if name == ".git" {
            continue;
        }
        let dest = to.join(&name);
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &dest)?;
        } else {
            std::fs::copy(entry.path(), dest)?;
        }
    }
    Ok(())
}

fn find_skill_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let walker = ignore::WalkBuilder::new(root)
        .hidden(false)
        .git_ignore(false)
        .max_depth(Some(5))
        .build();
    for e in walker.flatten() {
        let p = e.path();
        if p.is_file()
            && p.file_name().and_then(|n| n.to_str()) == Some("SKILL.md")
            && !p.components().any(|c| c.as_os_str() == ".git")
        {
            out.push(p.to_path_buf());
        }
    }
    out.sort();
    out
}

/// Clone the source and copy every skill folder found into `target`.
pub async fn install(source: &Source, target: &Path) -> Result<Vec<Installed>, String> {
    let tmp = std::env::temp_dir().join(format!("lz-skill-{}", now_ms()));
    let mut cmd = tokio::process::Command::new("git");
    cmd.args(["clone", "--depth", "1", "--quiet"]);
    if let Some(r) = &source.git_ref {
        cmd.args(["--branch", r]);
    }
    cmd.arg(&source.url).arg(&tmp);
    cmd.stdin(std::process::Stdio::null());
    let out = cmd
        .output()
        .await
        .map_err(|e| format!("git is required to install skills: {e}"))?;
    if !out.status.success() {
        let _ = std::fs::remove_dir_all(&tmp);
        let msg = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(format!("git clone failed for {}: {msg}", source.url));
    }
    let root = match &source.subpath {
        Some(p) => tmp.join(p),
        None => tmp.clone(),
    };
    if !root.is_dir() {
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(format!(
            "path `{}` does not exist in {}",
            source.subpath.clone().unwrap_or_default(),
            source.url
        ));
    }
    let files = find_skill_files(&root);
    if files.is_empty() {
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(format!(
            "no SKILL.md found in {}{}",
            source.url,
            source
                .subpath
                .as_ref()
                .map(|p| format!(" under {p}"))
                .unwrap_or_default()
        ));
    }
    let mut installed = Vec::new();
    for file in files {
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        let Ok(md) = markdown::parse(&text) else { continue };
        let Some(name) = md.data.get("name").and_then(|v| v.as_str()).map(str::to_string) else {
            continue;
        };
        let description = md
            .data
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let skill_dir = file.parent().unwrap_or(&root);
        let dest = target.join(&name);
        if dest.exists() {
            std::fs::remove_dir_all(&dest).map_err(|e| e.to_string())?;
        }
        copy_dir(skill_dir, &dest).map_err(|e| e.to_string())?;
        let rel_sub = skill_dir
            .strip_prefix(&tmp)
            .ok()
            .map(|p| p.display().to_string())
            .filter(|p| !p.is_empty());
        let meta = InstalledMeta {
            source: source.url.clone(),
            git_ref: source.git_ref.clone(),
            subpath: rel_sub,
            installed_at: now_ms(),
        };
        let _ = std::fs::write(
            dest.join(META_FILE),
            serde_json::to_string_pretty(&meta).unwrap_or_default(),
        );
        installed.push(Installed {
            name,
            description,
            path: dest,
        });
    }
    let _ = std::fs::remove_dir_all(&tmp);
    if installed.is_empty() {
        return Err("SKILL.md found but none had a `name` in its frontmatter".into());
    }
    Ok(installed)
}

/// Skills installed under `dir` (those with our metadata file).
pub fn installed(dir: &Path) -> Vec<(String, InstalledMeta, PathBuf)> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    for e in rd.flatten() {
        let p = e.path();
        if let Ok(text) = std::fs::read_to_string(p.join(META_FILE))
            && let Ok(meta) = serde_json::from_str::<InstalledMeta>(&text)
            && let Some(name) = p.file_name().and_then(|n| n.to_str())
        {
            out.push((name.to_string(), meta, p.clone()));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

pub fn remove(paths: &Paths, worktree: &Path, name: &str) -> Result<PathBuf, String> {
    for global in [false, true] {
        let dir = target_dir(paths, worktree, global).join(name);
        if dir.join(META_FILE).exists() || dir.join("SKILL.md").exists() {
            std::fs::remove_dir_all(&dir).map_err(|e| e.to_string())?;
            return Ok(dir);
        }
    }
    Err(format!("no installed skill named `{name}`"))
}

/// Make sure every `skills.urls` entry is installed (global scope).
pub async fn sync_urls(paths: &Paths, urls: &[String]) -> Vec<Result<Vec<Installed>, String>> {
    let dir = paths.config.join("skills");
    let have = installed(&dir);
    let mut out = Vec::new();
    for u in urls {
        let Ok(src) = Source::parse(u) else {
            out.push(Err(format!("bad skills.urls entry: {u}")));
            continue;
        };
        let present = have.iter().any(|(_, m, _)| {
            m.source == src.url
                && m.subpath
                    .as_deref()
                    .unwrap_or("")
                    .starts_with(src.subpath.as_deref().unwrap_or(""))
        });
        if present {
            continue;
        }
        out.push(install(&src, &dir).await);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_common_forms() {
        let s = Source::parse("owner/repo").unwrap();
        assert_eq!(s.url, "https://github.com/owner/repo.git");
        assert_eq!(s.subpath, None);
        let s = Source::parse("https://github.com/owner/repo/tree/main/skills/pdf").unwrap();
        assert_eq!(s.git_ref.as_deref(), Some("main"));
        assert_eq!(s.subpath.as_deref(), Some("skills/pdf"));
        let s = Source::parse("https://github.com/owner/repo/blob/v2/skills/pdf/SKILL.md").unwrap();
        assert_eq!(s.subpath.as_deref(), Some("skills/pdf"));
        let s = Source::parse("git@github.com:owner/repo.git#dev").unwrap();
        assert_eq!(s.url, "git@github.com:owner/repo.git");
        assert_eq!(s.git_ref.as_deref(), Some("dev"));
        assert!(Source::parse("nonsense").is_err());
    }
}
