//! models.dev catalog: fetched and cached, with an embedded gzipped snapshot
//! as the last-resort fallback.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};
use serde_json::Value;

const EMBEDDED: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../assets/models.json.gz"
));
const DEFAULT_URLS: &[&str] = &[
    "https://models.opencode.ai/api.json",
    "https://models.dev/api.json",
];
const CACHE_TTL: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct CatalogCost {
    #[serde(default)]
    pub input: f64,
    #[serde(default)]
    pub output: f64,
    #[serde(default)]
    pub cache_read: Option<f64>,
    #[serde(default)]
    pub cache_write: Option<f64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct CatalogLimit {
    #[serde(default)]
    pub context: f64,
    #[serde(default)]
    pub input: Option<f64>,
    #[serde(default)]
    pub output: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct CatalogModalities {
    #[serde(default)]
    pub input: Vec<String>,
    #[serde(default)]
    pub output: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ReasoningOption {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub values: Vec<Value>,
    #[serde(default)]
    pub min: Option<i64>,
    #[serde(default)]
    pub max: Option<i64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct CatalogModelProvider {
    #[serde(default)]
    pub npm: Option<String>,
    #[serde(default)]
    pub api: Option<String>,
    #[serde(default)]
    pub shape: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct CatalogModel {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub family: Option<String>,
    #[serde(default)]
    pub attachment: bool,
    #[serde(default)]
    pub reasoning: bool,
    #[serde(default)]
    pub reasoning_options: Option<Vec<ReasoningOption>>,
    #[serde(default)]
    pub tool_call: bool,
    #[serde(default = "default_true")]
    pub temperature: bool,
    #[serde(default)]
    pub release_date: Option<String>,
    #[serde(default)]
    pub modalities: Option<CatalogModalities>,
    #[serde(default)]
    pub limit: CatalogLimit,
    #[serde(default)]
    pub cost: Option<CatalogCost>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub provider: Option<CatalogModelProvider>,
    #[serde(default)]
    pub options: Option<serde_json::Map<String, Value>>,
    #[serde(default)]
    pub headers: Option<BTreeMap<String, String>>,
    #[serde(default)]
    pub experimental: Option<Value>,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct CatalogProvider {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub env: Vec<String>,
    #[serde(default)]
    pub npm: Option<String>,
    #[serde(default)]
    pub api: Option<String>,
    #[serde(default)]
    pub models: BTreeMap<String, CatalogModel>,
}

pub type Catalog = BTreeMap<String, CatalogProvider>;

pub fn parse(bytes: &[u8]) -> Result<Catalog, serde_json::Error> {
    serde_json::from_slice(bytes)
}

pub fn embedded() -> Catalog {
    let mut out = Vec::new();
    let mut gz = flate2::read::GzDecoder::new(EMBEDDED);
    if gz.read_to_end(&mut out).is_err() {
        return Catalog::new();
    }
    parse(&out).unwrap_or_default()
}

fn cache_is_fresh(path: &Path) -> bool {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| SystemTime::now().duration_since(t).ok())
        .is_some_and(|age| age < CACHE_TTL)
}

/// Load the catalog: fresh cache → embedded (and refresh in background) or
/// network when `refresh` is forced / cache stale.
pub async fn load(cache: &Path, refresh: bool) -> Catalog {
    if std::env::var("LZ_DISABLE_MODELS_FETCH").is_ok()
        || std::env::var("OPENCODE_DISABLE_MODELS_FETCH").is_ok()
    {
        return from_cache_or_embedded(cache);
    }
    if !refresh
        && cache_is_fresh(cache)
        && let Some(c) = from_cache(cache)
    {
        return c;
    }
    if let Some(c) = fetch(cache).await {
        return c;
    }
    from_cache_or_embedded(cache)
}

fn from_cache(cache: &Path) -> Option<Catalog> {
    let bytes = std::fs::read(cache).ok()?;
    parse(&bytes).ok().filter(|c| !c.is_empty())
}

fn from_cache_or_embedded(cache: &Path) -> Catalog {
    from_cache(cache).unwrap_or_else(embedded)
}

async fn fetch(cache: &Path) -> Option<Catalog> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .ok()?;
    let urls: Vec<String> = std::env::var("LZ_MODELS_URL")
        .ok()
        .or_else(|| std::env::var("OPENCODE_MODELS_URL").ok())
        .map(|u| vec![u])
        .unwrap_or_else(|| DEFAULT_URLS.iter().map(|s| s.to_string()).collect());
    for url in urls {
        let Ok(resp) = client.get(&url).send().await else {
            continue;
        };
        if !resp.status().is_success() {
            continue;
        }
        let Ok(bytes) = resp.bytes().await else { continue };
        let Ok(catalog) = parse(&bytes) else { continue };
        if catalog.is_empty() {
            continue;
        }
        if let Some(parent) = cache.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(cache, &bytes);
        tracing::debug!(url, providers = catalog.len(), "models catalog refreshed");
        return Some(catalog);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_snapshot_parses() {
        let c = embedded();
        assert!(c.len() > 100);
        assert!(c["openai"].models.contains_key("gpt-4o"));
        assert_eq!(
            c["openrouter"].api.as_deref(),
            Some("https://openrouter.ai/api/v1")
        );
    }
}
