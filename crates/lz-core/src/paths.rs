//! XDG-style directory layout. LunarZero owns `~/.config/lunarzero` etc.;
//! matching legacy directories are read as fallbacks for migration.

use std::path::{Path, PathBuf};

use etcetera::BaseStrategy;

#[derive(Debug, Clone)]
pub struct Paths {
    pub home: PathBuf,
    pub config: PathBuf,
    pub data: PathBuf,
    pub cache: PathBuf,
    pub state: PathBuf,
    /// Legacy config dir, read-only fallback for migrated setups.
    pub legacy_config: PathBuf,
    pub legacy_data: PathBuf,
}

/// Read `LZ_<name>` falling back to `OPENCODE_<name>`.
pub fn env_var(name: &str) -> Option<String> {
    std::env::var(format!("LZ_{name}"))
        .ok()
        .or_else(|| std::env::var(format!("OPENCODE_{name}")).ok())
        .filter(|v| !v.is_empty())
}

impl Paths {
    pub fn detect() -> Self {
        let base = etcetera::base_strategy::Xdg::new().ok();
        let home = etcetera::home_dir().unwrap_or_else(|_| PathBuf::from("/"));
        let (config_home, data_home, cache_home, state_home) = match &base {
            Some(b) => (
                b.config_dir(),
                b.data_dir(),
                b.cache_dir(),
                b.state_dir().unwrap_or_else(|| home.join(".local/state")),
            ),
            None => (
                home.join(".config"),
                home.join(".local/share"),
                home.join(".cache"),
                home.join(".local/state"),
            ),
        };
        let config = std::env::var("LZ_CONFIG_DIR")
            .ok()
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| config_home.join("lunarzero"));
        let legacy_config = std::env::var("OPENCODE_CONFIG_DIR")
            .ok()
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| config_home.join("opencode"));
        Self {
            config,
            data: data_home.join("lunarzero"),
            cache: cache_home.join("lunarzero"),
            state: state_home.join("lunarzero"),
            legacy_config,
            legacy_data: data_home.join("opencode"),
            home,
        }
    }

    pub fn ensure(&self) -> std::io::Result<()> {
        for d in [
            &self.config,
            &self.data,
            &self.cache,
            &self.state,
            &self.tool_output(),
            &self.log(),
        ] {
            std::fs::create_dir_all(d)?;
        }
        Ok(())
    }

    pub fn db(&self) -> PathBuf {
        env_var("DB")
            .map(PathBuf::from)
            .unwrap_or_else(|| self.data.join("lunarzero.db"))
    }
    pub fn auth(&self) -> PathBuf {
        self.data.join("auth.json")
    }
    pub fn legacy_auth(&self) -> PathBuf {
        self.legacy_data.join("auth.json")
    }
    pub fn mcp_auth(&self) -> PathBuf {
        self.data.join("mcp-auth.json")
    }
    pub fn tool_output(&self) -> PathBuf {
        self.data.join("tool-output")
    }
    pub fn snapshot(&self) -> PathBuf {
        self.data.join("snapshot")
    }
    pub fn plans(&self) -> PathBuf {
        self.data.join("plans")
    }
    pub fn log(&self) -> PathBuf {
        self.data.join("log")
    }
    pub fn models_cache(&self) -> PathBuf {
        self.cache.join("models.json")
    }
    pub fn kv(&self) -> PathBuf {
        self.state.join("kv.json")
    }

    /// Global config directories, lowest precedence first (legacy, then lunarzero).
    pub fn global_config_dirs(&self) -> Vec<PathBuf> {
        let mut v = Vec::new();
        if self.legacy_config != self.config {
            v.push(self.legacy_config.clone());
        }
        v.push(self.config.clone());
        v
    }
}

/// Expand a leading `~` or `$HOME`.
/// Models often hand us shell-escaped or slightly mangled paths
/// (`app/\(dashboard\)/\[id\)/page.tsx`, quotes, `./`). Undo the escaping
/// and repair an obviously mismatched bracket so files land where intended
/// instead of in a directory literally named `\(dashboard\)`.
pub fn normalize_model_path(p: &str) -> String {
    let mut s = p.trim().to_string();
    if (s.starts_with('"') && s.ends_with('"') || s.starts_with('\'') && s.ends_with('\'')) && s.len() >= 2 {
        s = s[1..s.len() - 1].to_string();
    }
    // unescape backslash-escaped punctuation and spaces
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\'
            && let Some(&n) = chars.peek()
            && matches!(
                n,
                '(' | ')' | '[' | ']' | '{' | '}' | ' ' | '$' | '&' | '\'' | '"' | '@' | '!'
            )
        {
            out.push(n);
            chars.next();
        } else {
            out.push(c);
        }
    }
    // repair segments like `[id)` / `(group]` (a paired opener with the wrong closer)
    let fixed: Vec<String> = out
        .split('/')
        .map(|seg| {
            let b = seg.as_bytes();
            if b.len() >= 2 {
                match (b[0], b[b.len() - 1]) {
                    (b'[', b')') => return format!("{}]", &seg[..seg.len() - 1]),
                    (b'(', b']') => return format!("{})", &seg[..seg.len() - 1]),
                    _ => {}
                }
            }
            seg.to_string()
        })
        .collect();
    fixed.join("/")
}

pub fn expand_home(p: &str, home: &Path) -> PathBuf {
    if let Some(rest) = p.strip_prefix("~/") {
        home.join(rest)
    } else if p == "~" {
        home.to_path_buf()
    } else if let Some(rest) = p.strip_prefix("$HOME/") {
        home.join(rest)
    } else {
        PathBuf::from(p)
    }
}

#[cfg(test)]
mod path_tests {
    use super::*;

    #[test]
    fn unescapes_and_repairs_model_paths() {
        assert_eq!(
            normalize_model_path("app/\\(dashboard\\)/project/\\[id\\)/page.tsx"),
            "app/(dashboard)/project/[id]/page.tsx"
        );
        assert_eq!(normalize_model_path("\"src/my file.rs\""), "src/my file.rs");
        assert_eq!(
            normalize_model_path("app/api/auth/[...nextauth]/route.ts"),
            "app/api/auth/[...nextauth]/route.ts"
        );
        assert_eq!(normalize_model_path("src\\ dir/x.py"), "src dir/x.py");
    }
}
