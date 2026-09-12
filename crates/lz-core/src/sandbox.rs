//! Least-privilege execution for the build step of installed skills and MCP
//! servers — freshly cloned, unreviewed third-party code.
//!
//! Two layers, both cheap:
//! 1. a scrubbed environment: no `*_KEY` / `*TOKEN*` / `*SECRET*` variables,
//!    `HOME` pointed at a private directory (so `~/.npmrc`, `~/.netrc`,
//!    `~/.ssh` are not where the code expects them), package caches kept;
//! 2. an OS sandbox when one exists: `sandbox-exec` on macOS, `bwrap` on
//!    Linux, denying reads of credential locations by absolute path — the
//!    main LunarZero `auth.json` included.
//!
//! With no OS sandbox available, package-manager lifecycle scripts are
//! skipped (`npm install --ignore-scripts`) so nothing runs at install time
//! beyond the build we invoke explicitly.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use crate::paths::Paths;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Isolation {
    /// sandbox-exec / bwrap wraps the process
    Os,
    /// environment scrubbing only
    EnvOnly,
}

pub struct Sandbox {
    pub isolation: Isolation,
    home: PathBuf,
    real_home: PathBuf,
    /// paths the build must not read
    protected: Vec<PathBuf>,
}

fn secret_var(name: &str) -> bool {
    let n = name.to_ascii_uppercase();
    (n.ends_with("_KEY") && n != "SSH_AUTH_SOCK")
        || n.contains("TOKEN")
        || n.contains("SECRET")
        || n.contains("PASSWORD")
        || n.contains("PASSWD")
        || n.contains("CREDENTIAL")
        || n.ends_with("_AUTH")
        || n.starts_with("AWS_")
        || n.starts_with("AZURE_")
        || n.starts_with("GH_")
        || n.starts_with("GITHUB_")
        || n == "LZ_AUTH_CONTENT"
        || n == "NPM_CONFIG__AUTH"
        || n == "NPM_CONFIG_AUTHTOKEN"
}

/// bubblewrap arguments: the whole filesystem visible, credential
/// locations replaced by empty tmpfs (directories) or /dev/null (files).
pub fn bwrap_args(protected: &[PathBuf]) -> Vec<String> {
    let mut a: Vec<String> = ["--dev-bind", "/", "/", "--die-with-parent", "--unshare-pid"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    for path in protected {
        let p = path.display().to_string();
        if path.is_dir() {
            a.push("--tmpfs".into());
            a.push(p);
        } else if path.is_file() {
            a.push("--ro-bind".into());
            a.push("/dev/null".into());
            a.push(p);
        }
    }
    a
}

impl Sandbox {
    pub fn new(paths: &Paths) -> Self {
        let real_home = paths.home.clone();
        let home = paths.cache.join("sandbox-home");
        let _ = std::fs::create_dir_all(&home);
        let mut protected = vec![
            paths.data.join("auth.json"),
            paths.data.join("mcp-auth.json"),
            real_home.join(".ssh"),
            real_home.join(".aws"),
            real_home.join(".azure"),
            real_home.join(".gnupg"),
            real_home.join(".netrc"),
            real_home.join(".npmrc"),
            real_home.join(".pypirc"),
            real_home.join(".docker/config.json"),
            real_home.join(".kube"),
            real_home.join(".config/gh"),
            real_home.join(".config/gcloud"),
            real_home.join(".git-credentials"),
        ];
        if cfg!(target_os = "macos") {
            protected.push(real_home.join("Library/Keychains"));
            protected.push(real_home.join("Library/Application Support/lunarzero"));
        }
        // seatbelt and bwrap match real paths: resolve symlinks (/var → /private/var)
        let protected: Vec<PathBuf> = protected
            .into_iter()
            .map(|p| {
                if p.exists() {
                    std::fs::canonicalize(&p).unwrap_or(p)
                } else if let (Some(parent), Some(name)) = (p.parent(), p.file_name())
                    && let Ok(cp) = std::fs::canonicalize(parent)
                {
                    cp.join(name)
                } else {
                    p
                }
            })
            .collect();
        let has_os_sandbox = (cfg!(target_os = "macos") && Path::new("/usr/bin/sandbox-exec").exists())
            || (cfg!(target_os = "linux") && crate::lsp::servers::on_path("bwrap"));
        let isolation = if has_os_sandbox {
            Isolation::Os
        } else {
            Isolation::EnvOnly
        };
        Self {
            isolation,
            home,
            real_home,
            protected,
        }
    }

    /// Environment for the build process.
    pub fn env(&self) -> Vec<(String, String)> {
        let mut env: Vec<(String, String)> = std::env::vars().filter(|(k, _)| !secret_var(k)).collect();
        let set = |env: &mut Vec<(String, String)>, k: &str, v: String| {
            env.retain(|(ek, _)| ek != k);
            env.push((k.to_string(), v));
        };
        set(&mut env, "HOME", self.home.display().to_string());
        // keep the real package caches so builds stay fast and offline-friendly
        let rh = &self.real_home;
        if std::env::var_os("CARGO_HOME").is_none() {
            set(&mut env, "CARGO_HOME", rh.join(".cargo").display().to_string());
        }
        if std::env::var_os("RUSTUP_HOME").is_none() {
            set(&mut env, "RUSTUP_HOME", rh.join(".rustup").display().to_string());
        }
        set(
            &mut env,
            "npm_config_cache",
            rh.join(".npm").display().to_string(),
        );
        set(
            &mut env,
            "UV_CACHE_DIR",
            rh.join(".cache/uv").display().to_string(),
        );
        set(
            &mut env,
            "GOPATH",
            std::env::var("GOPATH").unwrap_or_else(|_| rh.join("go").display().to_string()),
        );
        set(
            &mut env,
            "GOMODCACHE",
            rh.join("go/pkg/mod").display().to_string(),
        );
        set(&mut env, "LZ_SANDBOX", "1".into());
        env
    }

    /// macOS `sandbox-exec` profile: allow everything except the protected paths.
    fn seatbelt_profile(&self) -> String {
        let mut p = String::from("(version 1)\n(allow default)\n");
        for path in &self.protected {
            let s = path.display().to_string().replace('"', "\\\"");
            p.push_str(&format!("(deny file-read* file-write* (subpath \"{s}\"))\n"));
        }
        p
    }

    /// Wrap `cmd args` in the OS sandbox when available.
    pub fn command(&self, cmd: &str, args: &[&str], cwd: &Path) -> tokio::process::Command {
        let mut c = match self.isolation {
            Isolation::Os if cfg!(target_os = "macos") => {
                let mut c = tokio::process::Command::new("/usr/bin/sandbox-exec");
                c.arg("-p").arg(self.seatbelt_profile()).arg(cmd).args(args);
                c
            }
            Isolation::Os => {
                let mut c = tokio::process::Command::new("bwrap");
                c.args(bwrap_args(&self.protected));
                c.arg("--").arg(cmd).args(args);
                c
            }
            Isolation::EnvOnly => {
                let mut c = tokio::process::Command::new(cmd);
                c.args(args);
                c
            }
        };
        c.current_dir(cwd)
            .env_clear()
            .envs(self.env())
            .stdin(Stdio::null());
        c
    }

    /// Extra flags for `npm install` / `pnpm install` / `yarn` when scripts
    /// cannot be contained by an OS sandbox.
    pub fn npm_install_flags(&self) -> Vec<&'static str> {
        match self.isolation {
            Isolation::Os => Vec::new(),
            Isolation::EnvOnly => vec!["--ignore-scripts"],
        }
    }

    pub fn describe(&self) -> String {
        match self.isolation {
            Isolation::Os if cfg!(target_os = "macos") => {
                "sandbox-exec: credentials, keys and auth.json unreadable; secrets scrubbed from env".into()
            }
            Isolation::Os => {
                "bwrap: credentials, keys and auth.json masked; secrets scrubbed from env".into()
            }
            Isolation::EnvOnly => {
                "no OS sandbox on this system: secrets scrubbed from env, package scripts skipped".into()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrubs_secret_variables() {
        assert!(secret_var("OPENAI_API_KEY"));
        assert!(secret_var("GITHUB_TOKEN"));
        assert!(secret_var("AWS_SECRET_ACCESS_KEY"));
        assert!(secret_var("LZ_AUTH_CONTENT"));
        assert!(secret_var("npm_config_authToken"));
        assert!(!secret_var("PATH"));
        assert!(!secret_var("HOME"));
        assert!(!secret_var("SSH_AUTH_SOCK"));
        assert!(!secret_var("CARGO_HOME"));
    }

    #[test]
    fn bwrap_masks_only_existing_paths() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("auth.json");
        std::fs::write(&f, "x").unwrap();
        let d = dir.path().join("ssh");
        std::fs::create_dir(&d).unwrap();
        let missing = dir.path().join("nope");
        let a = bwrap_args(&[f.clone(), d.clone(), missing]);
        assert_eq!(
            &a[..5],
            &["--dev-bind", "/", "/", "--die-with-parent", "--unshare-pid"]
        );
        assert!(
            a.windows(3)
                .any(|w| w == ["--ro-bind", "/dev/null", &f.display().to_string()])
        );
        assert!(a.windows(2).any(|w| w == ["--tmpfs", &d.display().to_string()]));
        assert!(!a.iter().any(|x| x.contains("nope")));
    }

    #[tokio::test]
    async fn build_cannot_read_auth_json() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted(dir.path());
        std::fs::create_dir_all(&paths.data).unwrap();
        let auth = paths.data.join("auth.json");
        std::fs::write(&auth, "{\"secret\":1}").unwrap();
        let sb = Sandbox::new(&paths);
        // env layer
        let env = sb.env();
        assert!(
            env.iter()
                .any(|(k, v)| k == "HOME" && v != &paths.home.display().to_string())
        );
        assert!(!env.iter().any(|(k, _)| k == "LZ_AUTH_CONTENT"));
        // OS layer (where available): the file must be unreadable
        if sb.isolation == Isolation::Os {
            let out = sb
                .command("cat", &[&auth.display().to_string()], dir.path())
                .output()
                .await
                .unwrap();
            assert!(!out.status.success(), "auth.json was readable inside the sandbox");
            // but ordinary work is not affected
            let ok = sb.command("echo", &["hi"], dir.path()).output().await.unwrap();
            assert!(ok.status.success());
        }
    }
}
