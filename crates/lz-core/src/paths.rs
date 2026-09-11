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
