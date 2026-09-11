//! The free-tier pool: LunarZero's own catalog of free models
//! (`assets/pool.json`, built by `scripts/build_pool.py` from models.dev
//! metadata with computed quality/speed scores and published rate limits),
//! layered onto the registry, plus the virtual `lunar` provider whose
//! models are resolved per request by [`super::router`].

use std::collections::BTreeMap;

use lz_schema::api::PoolInfo;
use lz_schema::config::{Config, PoolConfig};
use serde::Deserialize;

use super::{Model, Provider, protocol_for, transform};

pub const PROVIDER: &str = "lunar";
pub const MODELS: &[(&str, &str, &str)] = &[
    (
        "auto",
        "Lunar auto",
        "Best free model for each request: quality, speed and remaining quota",
    ),
    (
        "smart",
        "Lunar smart",
        "Highest-quality free model that is under its limits",
    ),
    (
        "fast",
        "Lunar fast",
        "Fastest free model that is under its limits",
    ),
];

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Limits {
    pub rpm: Option<u64>,
    pub rpd: Option<u64>,
    pub tpm: Option<u64>,
    pub tpd: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PoolProvider {
    pub name: String,
    pub signup: String,
    pub env: Vec<String>,
    pub base_url: String,
    #[serde(default)]
    pub note: String,
    /// static speed prior 0–1
    #[serde(default)]
    pub speed: f64,
    #[serde(default)]
    pub limits: Limits,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PoolModel {
    pub provider: String,
    pub id: String,
    pub name: String,
    pub quality: u32,
    pub speed: u32,
    pub context: f64,
    pub tools: bool,
    pub vision: bool,
    pub reasoning: bool,
    #[serde(default)]
    pub params_b: f64,
    #[serde(default)]
    pub active_b: f64,
    /// model-specific caps; otherwise the provider defaults apply
    #[serde(default)]
    pub limits: Option<Limits>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PoolCatalog {
    pub version: String,
    pub method: String,
    pub providers: BTreeMap<String, PoolProvider>,
    pub models: Vec<PoolModel>,
}

impl PoolCatalog {
    pub fn info(&self, m: &PoolModel) -> PoolInfo {
        let base = self
            .providers
            .get(&m.provider)
            .map(|p| p.limits.clone())
            .unwrap_or_default();
        let l = m.limits.clone().unwrap_or(base);
        PoolInfo {
            quality: m.quality,
            speed: m.speed,
            rpm: l.rpm,
            rpd: l.rpd,
            tpm: l.tpm,
            tpd: l.tpd,
        }
    }
}

static EMBEDDED: &str = include_str!("../../../../assets/pool.json");

pub fn catalog() -> &'static PoolCatalog {
    static CAT: std::sync::LazyLock<PoolCatalog> =
        std::sync::LazyLock::new(|| serde_json::from_str(EMBEDDED).expect("embedded pool catalog"));
    &CAT
}

pub fn config(config: &Config) -> PoolConfig {
    config.pool.clone().unwrap_or_default()
}

pub fn enabled(config: &Config) -> bool {
    self::config(config).enabled.unwrap_or(true)
}

/// `exclude` patterns: `provider/model` or `provider/*`.
fn excluded(cfg: &PoolConfig, provider: &str, model: &str) -> bool {
    cfg.exclude.as_ref().is_some_and(|list| {
        list.iter().any(|pat| {
            pat == &format!("{provider}/{model}") || pat == &format!("{provider}/*") || pat == provider
        })
    })
}

/// A usable key for `provider` from the pool's env vars.
pub fn env_key(provider_id: &str) -> Option<String> {
    let p = catalog().providers.get(provider_id)?;
    p.env
        .iter()
        .find_map(|v| std::env::var(v).ok().filter(|s| !s.is_empty()))
}

fn blank_model(provider: &Provider, id: &str, name: &str, context: f64) -> Model {
    Model {
        provider_id: provider.id.clone(),
        id: id.into(),
        api_id: id.into(),
        name: name.into(),
        family: None,
        npm: provider.npm.clone(),
        base_url: provider.base_url.clone(),
        reasoning: false,
        tool_call: true,
        attachment: false,
        temperature: true,
        cost: Default::default(),
        limit: lz_schema::api::ModelLimit {
            context,
            input: None,
            output: 8192.0,
        },
        status: None,
        variants: BTreeMap::new(),
        options: Default::default(),
        headers: Vec::new(),
        protocol: Some("openai-chat"),
        url_overridden: false,
        pool: None,
    }
}

/// Layer the pool onto the registry providers (called from
/// `Registry::build_with` after config/env/auth were applied).
pub fn apply(providers: &mut BTreeMap<String, Provider>, config: &Config, all_models: bool) {
    if !enabled(config) {
        return;
    }
    let cfg = self::config(config);
    let cat = catalog();
    // 1. providers models.dev lacks, or whose native SDK we don't speak: use the OpenAI-compatible endpoint
    for (id, pp) in &cat.providers {
        let entry = providers.entry(id.clone()).or_insert_with(|| Provider {
            id: id.clone(),
            name: pp.name.clone(),
            npm: "@ai-sdk/openai-compatible".into(),
            base_url: super::expand_env(&pp.base_url),
            api_key: None,
            headers: Vec::new(),
            source: "catalog",
            models: BTreeMap::new(),
            reachable: false,
        });
        if entry.api_key.is_none() {
            entry.api_key = env_key(id);
            if entry.api_key.is_some() && entry.source == "catalog" {
                entry.source = "env";
            }
        }
        if protocol_for(&entry.npm).is_none() || entry.base_url.is_empty() {
            entry.base_url = super::expand_env(&pp.base_url);
            entry.npm = "@ai-sdk/openai-compatible".into();
            for m in entry.models.values_mut() {
                if m.protocol.is_none() {
                    m.protocol = Some("openai-chat");
                    m.npm = entry.npm.clone();
                }
                if !m.url_overridden {
                    m.base_url = entry.base_url.clone();
                }
            }
        }
    }
    // 2. pool members
    let mut any_connected = false;
    for pm in &cat.models {
        if excluded(&cfg, &pm.provider, &pm.id) {
            continue;
        }
        let Some(p) = providers.get_mut(&pm.provider) else {
            continue;
        };
        let connected = p.connected();
        if !connected && !all_models {
            continue;
        }
        any_connected |= connected;
        let template = blank_model(p, &pm.id, &pm.name, pm.context);
        let model = p.models.entry(pm.id.clone()).or_insert(template);
        if model.protocol.is_none() {
            model.protocol = Some("openai-chat");
        }
        model.tool_call = pm.tools;
        model.attachment |= pm.vision;
        model.reasoning |= pm.reasoning;
        model.pool = Some(cat.info(pm));
        if model.variants.is_empty() {
            model.variants = transform::variants(model, None);
        }
    }
    // 3. user-listed extra members
    if let Some(extra) = &cfg.include {
        for spec in extra {
            if let Some((pid, mid)) = spec.split_once('/')
                && let Some(p) = providers.get_mut(pid)
                && let Some(m) = p.models.get_mut(mid)
            {
                m.pool.get_or_insert(PoolInfo {
                    quality: 50,
                    speed: 50,
                    ..Default::default()
                });
                any_connected |= p.connected();
            }
        }
    }
    // 4. the virtual `lunar` provider
    if any_connected || all_models {
        let mut lunar = Provider {
            id: PROVIDER.into(),
            name: "Lunar (free pool)".into(),
            npm: "@ai-sdk/openai-compatible".into(),
            base_url: String::new(),
            api_key: if any_connected { Some("pool".into()) } else { None },
            headers: Vec::new(),
            source: "pool",
            models: BTreeMap::new(),
            reachable: true,
        };
        for (id, name, _desc) in MODELS {
            let mut m = blank_model(&lunar, id, name, 128_000.0);
            m.family = Some("lunar".into());
            m.pool = Some(PoolInfo::default());
            lunar.models.insert(id.to_string(), m);
        }
        providers.insert(PROVIDER.into(), lunar);
    }
}

pub fn is_virtual(model: &Model) -> bool {
    model.provider_id == PROVIDER
}

/// Strategy encoded in a `lunar/*` model id, else the configured default.
pub fn strategy_of(model: &Model, config: &Config) -> Strategy {
    let explicit = match model.id.as_str() {
        "fast" | "smart" => Some(model.id.clone()),
        _ => None,
    };
    let s = explicit
        .or_else(|| self::config(config).strategy.clone())
        .unwrap_or_else(|| "auto".into());
    Strategy::parse(&s)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Strategy {
    Auto,
    Fast,
    Smart,
}

impl Strategy {
    pub fn parse(s: &str) -> Strategy {
        match s {
            "fast" => Strategy::Fast,
            "smart" => Strategy::Smart,
            _ => Strategy::Auto,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Strategy::Auto => "auto",
            Strategy::Fast => "fast",
            Strategy::Smart => "smart",
        }
    }
}
