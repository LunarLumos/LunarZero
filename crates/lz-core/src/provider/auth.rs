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
        out
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
        current.remove(provider);
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
