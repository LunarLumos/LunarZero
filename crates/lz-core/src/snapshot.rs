//! Filesystem snapshots via a shadow git repository per worktree
//! (`git --git-dir <data>/snapshot/<project>/<hash> --work-tree <root>`).
//! Used for per-step change tracking and `/undo`.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use lz_schema::session::{FileDiff, FileDiffStatus};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;

use crate::paths::Paths;

const CORE: &[&str] = &[
    "-c",
    "core.longpaths=true",
    "-c",
    "core.symlinks=true",
    "-c",
    "core.autocrlf=false",
];

pub struct Snapshot {
    gitdir: PathBuf,
    worktree: PathBuf,
    enabled: bool,
    lock: Mutex<()>,
    initialized: std::sync::atomic::AtomicBool,
}

struct Out {
    code: i32,
    text: String,
    stderr: String,
}

impl Snapshot {
    pub fn new(paths: &Paths, project_id: &str, worktree: &Path, is_git: bool, enabled: bool) -> Self {
        let mut h = Sha256::new();
        h.update(worktree.display().to_string().as_bytes());
        let hash = format!("{:x}", h.finalize());
        Self {
            gitdir: paths.snapshot().join(project_id).join(&hash[..16]),
            worktree: worktree.to_path_buf(),
            enabled: enabled && is_git,
            lock: Mutex::new(()),
            initialized: std::sync::atomic::AtomicBool::new(false),
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    async fn git(&self, args: &[&str], cwd: Option<&Path>) -> std::io::Result<Out> {
        let mut cmd = tokio::process::Command::new("git");
        cmd.args(CORE)
            .arg("--git-dir")
            .arg(&self.gitdir)
            .arg("--work-tree")
            .arg(&self.worktree)
            .args(args);
        cmd.current_dir(cwd.unwrap_or(&self.worktree))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let out = cmd.output().await?;
        Ok(Out {
            code: out.status.code().unwrap_or(-1),
            text: String::from_utf8_lossy(&out.stdout).to_string(),
            stderr: String::from_utf8_lossy(&out.stderr).to_string(),
        })
    }

    async fn ensure_init(&self) -> std::io::Result<()> {
        if self.initialized.load(std::sync::atomic::Ordering::Relaxed) {
            return Ok(());
        }
        if !self.gitdir.join("HEAD").exists() {
            std::fs::create_dir_all(&self.gitdir)?;
            let init = tokio::process::Command::new("git")
                .args(["init", "--bare", "-q"])
                .arg(&self.gitdir)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .await?;
            if !init.success() {
                return Err(std::io::Error::other("git init failed"));
            }
            for (k, v) in [
                ("core.autocrlf", "false"),
                ("core.longpaths", "true"),
                ("core.symlinks", "true"),
                ("core.fsmonitor", "false"),
                ("feature.manyFiles", "true"),
                ("index.version", "4"),
                ("core.untrackedCache", "true"),
            ] {
                let _ = tokio::process::Command::new("git")
                    .arg("--git-dir")
                    .arg(&self.gitdir)
                    .args(["config", k, v])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .await;
            }
            // honor the project's ignores
            let exclude = self.gitdir.join("info").join("exclude");
            let _ = std::fs::create_dir_all(exclude.parent().unwrap());
            let mut content = String::from(".git/\n");
            if let Ok(gi) = std::fs::read_to_string(self.worktree.join(".gitignore")) {
                content.push_str(&gi);
            }
            let _ = std::fs::write(&exclude, content);
        }
        self.initialized.store(true, std::sync::atomic::Ordering::Relaxed);
        Ok(())
    }

    /// Stage the whole worktree and return the tree hash.
    pub async fn track(&self) -> Option<String> {
        if !self.enabled {
            return None;
        }
        let _g = self.lock.lock().await;
        self.ensure_init().await.ok()?;
        let add = self.git(&["add", "--all", "--", "."], None).await.ok()?;
        if add.code != 0 {
            tracing::debug!("snapshot add failed: {}", add.stderr.trim());
            return None;
        }
        let tree = self.git(&["write-tree"], None).await.ok()?;
        if tree.code != 0 {
            return None;
        }
        Some(tree.text.trim().to_string())
    }

    /// Files changed between `hash` and the current index.
    pub async fn patch(&self, hash: &str) -> Option<(String, Vec<String>)> {
        if !self.enabled {
            return None;
        }
        let _g = self.lock.lock().await;
        let add = self.git(&["add", "--all", "--", "."], None).await.ok()?;
        if add.code != 0 {
            return None;
        }
        let out = self
            .git(
                &[
                    "diff",
                    "--cached",
                    "--no-ext-diff",
                    "--name-only",
                    hash,
                    "--",
                    ".",
                ],
                None,
            )
            .await
            .ok()?;
        if out.code != 0 {
            return None;
        }
        let files: Vec<String> = out
            .text
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| self.worktree.join(l.trim()).display().to_string())
            .collect();
        Some((hash.to_string(), files))
    }

    /// Per-file diffs between two trees (or `from` and the working tree when `to` is None).
    pub async fn diff(&self, from: &str, to: Option<&str>) -> Vec<FileDiff> {
        if !self.enabled {
            return Vec::new();
        }
        let _g = self.lock.lock().await;
        if to.is_none() {
            let _ = self.git(&["add", "--all", "--", "."], None).await;
        }
        let mut args = vec!["diff", "--no-ext-diff", "--no-renames", "--numstat", from];
        if let Some(t) = to {
            args.push(t);
        } else {
            args.insert(1, "--cached");
        }
        args.extend(["--", "."]);
        let Ok(numstat) = self.git(&args, None).await else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for line in numstat.text.lines() {
            let mut cols = line.split('\t');
            let (Some(a), Some(d), Some(file)) = (cols.next(), cols.next(), cols.next()) else {
                continue;
            };
            let binary = a == "-";
            let abs = self.worktree.join(file);
            let mut patch_args = vec!["diff", "--no-ext-diff", "--no-renames", from];
            if let Some(t) = to {
                patch_args.push(t);
            } else {
                patch_args.insert(1, "--cached");
            }
            patch_args.extend(["--", file]);
            let patch = if binary {
                String::new()
            } else {
                self.git(&patch_args, None)
                    .await
                    .map(|o| o.text)
                    .unwrap_or_default()
            };
            let status = if patch.contains("new file mode") {
                Some(FileDiffStatus::Added)
            } else if patch.contains("deleted file mode") {
                Some(FileDiffStatus::Deleted)
            } else {
                Some(FileDiffStatus::Modified)
            };
            out.push(FileDiff {
                file: Some(abs.display().to_string()),
                patch: Some(patch),
                additions: a.parse().unwrap_or(0.0),
                deletions: d.parse().unwrap_or(0.0),
                status,
            });
        }
        out
    }

    /// Restore the whole worktree to a tree hash.
    pub async fn restore(&self, hash: &str) -> Result<(), String> {
        if !self.enabled {
            return Ok(());
        }
        let _g = self.lock.lock().await;
        let rt = self
            .git(&["read-tree", hash], None)
            .await
            .map_err(|e| e.to_string())?;
        if rt.code != 0 {
            return Err(format!("read-tree failed: {}", rt.stderr.trim()));
        }
        let co = self
            .git(&["checkout-index", "-a", "-f"], None)
            .await
            .map_err(|e| e.to_string())?;
        if co.code != 0 {
            return Err(format!("checkout-index failed: {}", co.stderr.trim()));
        }
        Ok(())
    }

    /// Put each file back to its state at the given snapshot; files that did
    /// not exist there are deleted.
    pub async fn revert(&self, patches: &[(String, Vec<String>)]) {
        if !self.enabled {
            return;
        }
        let _g = self.lock.lock().await;
        let mut seen = std::collections::HashSet::new();
        for (hash, files) in patches {
            for file in files {
                if !seen.insert(file.clone()) {
                    continue;
                }
                let rel = Path::new(file)
                    .strip_prefix(&self.worktree)
                    .map(|p| p.display().to_string())
                    .unwrap_or(file.clone());
                let co = self.git(&["checkout", hash, "--", file], None).await;
                if co.as_ref().is_ok_and(|o| o.code == 0) {
                    continue;
                }
                let tree = self.git(&["ls-tree", hash, "--", &rel], None).await;
                if tree
                    .as_ref()
                    .is_ok_and(|o| o.code == 0 && !o.text.trim().is_empty())
                {
                    continue;
                }
                let _ = std::fs::remove_file(file);
            }
        }
    }
}
