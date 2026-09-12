//! `auth.json` credential store (mode 0600). A legacy credential file is
//! read as a fallback so existing logins carry over.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use lz_schema::api::AuthInfo;

use crate::paths::{Paths, env_var};

#[derive(Debug, Clone, Default)]
pub struct AuthStore {
    path: PathBuf,
    fallback: Option<PathBuf>,
}

/// Placeholder stored in `auth.json` when the secret lives in the OS keychain
/// (macOS Keychain, Secret Service on Linux, Credential Manager on Windows).
pub const KEYCHAIN_REF: &str = "@keychain";
const KEYCHAIN_SERVICE: &str = "lunarzero";

fn keychain_entry(provider: &str) -> Option<keyring::Entry> {
    keyring::Entry::new(KEYCHAIN_SERVICE, provider).ok()
}

/// Is the OS keychain usable on this machine (a probe, cached per process)?
pub fn keychain_available() -> bool {
    static AVAILABLE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        let Some(e) = keychain_entry("lz-probe") else {
            return false;
        };
        let ok = match e.set_password("probe") {
            Ok(()) => true,
            Err(err) => {
                tracing::warn!("OS keychain unavailable: {err}");
                false
            }
        };
        let _ = e.delete_credential();
        ok
    })
}

impl AuthStore {
    pub fn new(paths: &Paths) -> Self {
        Self {
            path: paths.auth(),
            fallback: Some(paths.legacy_auth()),
        }
    }

    pub fn at(path: PathBuf) -> Self {
        Self { path, fallback: None }
    }

    fn read_file(path: &Path) -> BTreeMap<String, AuthInfo> {
        let Ok(text) = std::fs::read_to_string(path) else {
            return BTreeMap::new();
        };
        serde_json::from_str::<BTreeMap<String, serde_json::Value>>(&text)
            .map(|m| {
                m.into_iter()
                    .filter_map(|(k, v)| serde_json::from_value::<AuthInfo>(v).ok().map(|a| (k, a)))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// All credentials: env override > lunarzero file > legacy file.
    pub fn all(&self) -> BTreeMap<String, AuthInfo> {
        if let Some(content) = env_var("AUTH_CONTENT")
            && let Ok(m) = serde_json::from_str::<BTreeMap<String, AuthInfo>>(&content)
        {
            return m;
        }
        let mut out = self
            .fallback
            .as_ref()
            .map(|p| Self::read_file(p))
            .unwrap_or_default();
        out.extend(Self::read_file(&self.path));
        // resolve keychain references
        for (provider, info) in out.iter_mut() {
            if let AuthInfo::Api { key, .. } = info
                && key == KEYCHAIN_REF
            {
                match keychain_entry(provider).and_then(|e| e.get_password().ok()) {
                    Some(secret) => *key = secret,
                    None => key.clear(),
                }
            }
        }
        out.retain(|_, info| !matches!(info, AuthInfo::Api { key, .. } if key.is_empty()));
        out
    }

    /// Where a provider's secret is kept: `file`, `keychain`, `env`, or absent.
    pub fn location(&self, provider: &str) -> Option<&'static str> {
        if env_var("AUTH_CONTENT").is_some() {
            return Some("env");
        }
        let file = Self::read_file(&self.path);
        match file.get(provider) {
            Some(AuthInfo::Api { key, .. }) if key == KEYCHAIN_REF => Some("keychain"),
            Some(_) => Some("file"),
            None => self
                .fallback
                .as_ref()
                .filter(|p| Self::read_file(p).contains_key(provider))
                .map(|_| "file"),
        }
    }

    /// Store an API key in the OS keychain; `auth.json` keeps only a reference.
    pub fn set_in_keychain(&self, provider: &str, key: &str) -> std::io::Result<()> {
        let entry = keychain_entry(provider).ok_or_else(|| std::io::Error::other("keychain unavailable"))?;
        entry
            .set_password(key)
            .map_err(|e| std::io::Error::other(format!("keychain: {e}")))?;
        self.set(
            provider,
            AuthInfo::Api {
                key: KEYCHAIN_REF.into(),
                metadata: None,
            },
        )
    }

    /// Move every file-stored API key into the keychain. Returns the providers moved.
    pub fn migrate_to_keychain(&self) -> std::io::Result<Vec<String>> {
        let mut moved = Vec::new();
        for (provider, info) in Self::read_file(&self.path) {
            if let AuthInfo::Api { key, .. } = info
                && key != KEYCHAIN_REF
            {
                self.set_in_keychain(&provider, &key)?;
                moved.push(provider);
            }
        }
        Ok(moved)
    }

    pub fn get(&self, provider: &str) -> Option<AuthInfo> {
        self.all().remove(provider)
    }

    pub fn set(&self, provider: &str, info: AuthInfo) -> std::io::Result<()> {
        let mut current = Self::read_file(&self.path);
        current.insert(provider.to_string(), info);
        self.write(&current)
    }

    pub fn remove(&self, provider: &str) -> std::io::Result<()> {
        let mut current = Self::read_file(&self.path);
        if let Some(AuthInfo::Api { key, .. }) = current.remove(provider)
            && key == KEYCHAIN_REF
            && let Some(e) = keychain_entry(provider)
        {
            let _ = e.delete_credential();
        }
        self.write(&current)
    }

    fn write(&self, map: &BTreeMap<String, AuthInfo>) -> std::io::Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = serde_json::to_string_pretty(map).map_err(std::io::Error::other)?;
        std::fs::write(&self.path, text)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let store = AuthStore::at(dir.path().join("auth.json"));
        store
            .set(
                "openai",
                AuthInfo::Api {
                    key: "sk-1".into(),
                    metadata: None,
                },
            )
            .unwrap();
        assert!(matches!(store.get("openai"), Some(AuthInfo::Api { key, .. }) if key == "sk-1"));
        store.remove("openai").unwrap();
        assert!(store.get("openai").is_none());
    }
}
