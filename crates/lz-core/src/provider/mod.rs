//! Provider registry: merges the models.dev catalog, user `provider` config,
//! environment API keys and `auth.json` into resolved providers + models, and
//! maps each model to a wire protocol + endpoint.

pub mod auth;
pub mod catalog;
pub mod pool;
pub mod router;
pub mod transform;

use std::collections::BTreeMap;
use std::sync::Arc;

use lz_schema::api::{AuthInfo, ModelCost, ModelInfo, ModelLimit, ProviderInfo, ProvidersResponse};
use lz_schema::config::{Config, ProviderConfig};
use serde_json::{Map, Value};

use crate::llm::protocol::Protocol;
use crate::llm::{Endpoint, protocols};
use crate::paths::Paths;
use auth::AuthStore;
use catalog::{Catalog, CatalogModel, CatalogProvider};

/// Fully resolved model, ready to be called.
#[derive(Debug, Clone)]
pub struct Model {
    pub provider_id: String,
    pub id: String,
    /// Wire model id (may differ from `id` when config overrides it).
    pub api_id: String,
    pub name: String,
    pub family: Option<String>,
    pub npm: String,
    pub base_url: String,
    pub reasoning: bool,
    pub tool_call: bool,
    pub attachment: bool,
    pub temperature: bool,
    pub cost: ModelCost,
    pub limit: ModelLimit,
    pub status: Option<String>,
    /// variant id → provider options patch (e.g. `{"reasoningEffort":"high"}`)
    pub variants: BTreeMap<String, Map<String, Value>>,
    pub options: Map<String, Value>,
    pub headers: Vec<(String, String)>,
    pub protocol: Option<&'static str>,
    /// The catalog gave this model its own `provider.api`, so provider-level
    /// URL overrides must not clobber it.
    pub url_overridden: bool,
    /// Free-pool metadata (rank + rate limits) when the model is a pool member.
    pub pool: Option<lz_schema::api::PoolInfo>,
}

impl Model {
    pub fn full_id(&self) -> String {
        format!("{}/{}", self.provider_id, self.id)
    }
}

#[derive(Debug, Clone)]
pub struct Provider {
    pub id: String,
    pub name: String,
    pub npm: String,
    pub base_url: String,
    pub api_key: Option<String>,
    pub headers: Vec<(String, String)>,
    /// `env` | `config` | `auth` | `catalog`
    pub source: &'static str,
    pub models: BTreeMap<String, Model>,
    /// For local servers (Ollama, LM Studio, …): something is listening on the port.
    pub reachable: bool,
}

impl Provider {
    pub fn connected(&self) -> bool {
        if is_local(&self.base_url) {
            return self.reachable || (self.source == "config" && self.api_key.is_some());
        }
        self.api_key.is_some() || self.source == "config"
    }
}

/// Quick TCP probe (150 ms) so an installed-but-not-running local server is
/// not offered as a working provider.
pub fn probe_local(url: &str) -> bool {
    let rest = url.split("://").nth(1).unwrap_or(url);
    let hostport = rest.split('/').next().unwrap_or(rest);
    let (host, port) = match hostport.rsplit_once(':') {
        Some((h, p)) => (h, p.parse::<u16>().unwrap_or(80)),
        None => (hostport, 80),
    };
    let host = if host == "0.0.0.0" || host == "localhost" {
        "127.0.0.1"
    } else {
        host
    };
    let Ok(addrs) = std::net::ToSocketAddrs::to_socket_addrs(&(host, port)) else {
        return false;
    };
    addrs
        .into_iter()
        .take(2)
        .any(|a| std::net::TcpStream::connect_timeout(&a, std::time::Duration::from_millis(150)).is_ok())
}

fn is_local(url: &str) -> bool {
    url.contains("localhost") || url.contains("127.0.0.1") || url.contains("0.0.0.0")
}

/// Default base URLs for npm packages whose catalog entry has no `api`.
fn default_base_url(npm: &str, provider_id: &str) -> Option<&'static str> {
    Some(match (npm, provider_id) {
        ("@ai-sdk/openai", _) => "https://api.openai.com/v1",
        ("@ai-sdk/groq", _) => "https://api.groq.com/openai/v1",
        ("@ai-sdk/xai", _) => "https://api.x.ai/v1",
        ("@ai-sdk/mistral", _) => "https://api.mistral.ai/v1",
        ("@ai-sdk/cerebras", _) => "https://api.cerebras.ai/v1",
        ("@ai-sdk/togetherai", _) => "https://api.together.xyz/v1",
        ("@ai-sdk/deepinfra", _) => "https://api.deepinfra.com/v1/openai",
        ("@ai-sdk/perplexity", _) => "https://api.perplexity.ai",
        ("@openrouter/ai-sdk-provider", _) => "https://openrouter.ai/api/v1",
        (_, "ollama") => "http://127.0.0.1:11434/v1",
        (_, "lmstudio") => "http://127.0.0.1:1234/v1",
        _ => return None,
    })
}

/// Models that are not general chat/coding models (image, audio, video,
/// embeddings, computer-use, safety classifiers, …) are kept out of pickers
/// and automatic defaults; they can still be named explicitly in config.
pub fn is_chat_model(id: &str) -> bool {
    let l = id.to_lowercase();
    ![
        "computer-use",
        "embedding",
        "embed-",
        "-tts",
        "tts-",
        "whisper",
        "transcri",
        "image",
        "imagen",
        "veo",
        "lyria",
        "audio",
        "-live",
        "live-",
        "robotics",
        "guard",
        "safety",
        "moderation",
        "rerank",
        "ocr",
        "sora",
        "dall-e",
        "realtime",
    ]
    .iter()
    .any(|k| l.contains(k))
}

/// Map an npm package name to a wire protocol id we implement.
pub fn protocol_for(npm: &str) -> Option<&'static str> {
    match npm {
        "@ai-sdk/openai"
        | "@ai-sdk/openai-compatible"
        | "@ai-sdk/groq"
        | "@ai-sdk/xai"
        | "@ai-sdk/mistral"
        | "@ai-sdk/cerebras"
        | "@ai-sdk/togetherai"
        | "@ai-sdk/deepinfra"
        | "@ai-sdk/perplexity"
        | "@openrouter/ai-sdk-provider"
        | "@ai-sdk/azure"
        | "@ai-sdk/deepseek" => Some("openai-chat"),
        _ => None,
    }
}

/// Substitute `${VAR}` in URLs (catalog uses it for region/resource names).
fn expand_env(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find("${") {
        out.push_str(&rest[..i]);
        let after = &rest[i + 2..];
        let Some(j) = after.find('}') else {
            out.push_str(&rest[i..]);
            return out;
        };
        out.push_str(&std::env::var(&after[..j]).unwrap_or_default());
        rest = &after[j + 1..];
    }
    out.push_str(rest);
    out
}

fn build_model(
    provider_id: &str,
    provider_npm: &str,
    provider_url: &str,
    id: &str,
    m: &CatalogModel,
) -> Model {
    let npm = m
        .provider
        .as_ref()
        .and_then(|p| p.npm.clone())
        .unwrap_or_else(|| provider_npm.to_string());
    let own_url = m
        .provider
        .as_ref()
        .and_then(|p| p.api.clone())
        .map(|u| expand_env(&u));
    let url_overridden = own_url.is_some();
    let base_url = own_url.unwrap_or_else(|| provider_url.to_string());
    let cost = m.cost.as_ref().map(|c| ModelCost {
        input: c.input,
        output: c.output,
        cache_read: c.cache_read.unwrap_or(0.0),
        cache_write: c.cache_write.unwrap_or(0.0),
    });
    let limit = ModelLimit {
        context: if m.limit.context > 0.0 {
            m.limit.context
        } else {
            128_000.0
        },
        input: m.limit.input,
        output: if m.limit.output > 0.0 {
            m.limit.output
        } else {
            8192.0
        },
    };
    let mut model = Model {
        provider_id: provider_id.into(),
        id: id.into(),
        api_id: if m.id.is_empty() { id.into() } else { m.id.clone() },
        name: if m.name.is_empty() {
            id.into()
        } else {
            m.name.clone()
        },
        family: m.family.clone(),
        protocol: protocol_for(&npm),
        npm,
        base_url,
        reasoning: m.reasoning,
        tool_call: m.tool_call,
        attachment: m.attachment,
        temperature: m.temperature,
        cost: cost.unwrap_or_default(),
        limit,
        status: m.status.clone(),
        variants: BTreeMap::new(),
        options: m.options.clone().unwrap_or_default(),
        headers: m.headers.clone().unwrap_or_default().into_iter().collect(),
        url_overridden,
        pool: None,
    };
    model.variants = transform::variants(&model, m.reasoning_options.as_deref());
    model
}

fn apply_model_config(model: &mut Model, cfg: &lz_schema::config::ModelConfig) {
    if let Some(id) = &cfg.id {
        model.api_id = id.clone();
    }
    if let Some(n) = &cfg.name {
        model.name = n.clone();
    }
    if let Some(f) = &cfg.family {
        model.family = Some(f.clone());
    }
    if let Some(v) = cfg.reasoning {
        model.reasoning = v;
    }
    if let Some(v) = cfg.tool_call {
        model.tool_call = v;
    }
    if let Some(v) = cfg.attachment {
        model.attachment = v;
    }
    if let Some(v) = cfg.temperature {
        model.temperature = v;
    }
    if let Some(c) = &cfg.cost {
        model.cost = ModelCost {
            input: c.input,
            output: c.output,
            cache_read: c.cache_read.unwrap_or(0.0),
            cache_write: c.cache_write.unwrap_or(0.0),
        };
    }
    if let Some(l) = &cfg.limit {
        model.limit = ModelLimit {
            context: l.context,
            input: l.input,
            output: l.output,
        };
    }
    if let Some(s) = &cfg.status {
        model.status = Some(s.clone());
    }
    if let Some(p) = &cfg.provider {
        if let Some(npm) = &p.npm {
            model.npm = npm.clone();
            model.protocol = protocol_for(npm);
        }
        if let Some(api) = &p.api {
            model.base_url = expand_env(api);
            model.url_overridden = true;
        }
    }
    if let Some(o) = &cfg.options {
        for (k, v) in o {
            model.options.insert(k.clone(), v.clone());
        }
    }
    if let Some(h) = &cfg.headers {
        for (k, v) in h {
            if let Some(v) = v.as_str() {
                model.headers.push((k.clone(), v.to_string()));
            }
        }
    }
    if let Some(vs) = &cfg.variants {
        for (k, v) in vs {
            if let Value::Object(o) = v {
                model.variants.insert(k.clone(), o.clone());
            }
        }
    }
}

pub struct Registry {
    pub providers: BTreeMap<String, Provider>,
    client: crate::llm::LlmClient,
}

impl Registry {
    pub fn build(catalog: &Catalog, config: &Config, auth: &AuthStore) -> Self {
        Self::build_with(catalog, config, auth, false)
    }

    /// `all_models`: keep model tables for providers that are not connected
    /// (costs ~7k model entries of memory; only `lz models <provider>` needs it).
    pub fn build_with(catalog: &Catalog, config: &Config, auth: &AuthStore, all_models: bool) -> Self {
        let creds = auth.all();
        let disabled = config.disabled_providers.clone().unwrap_or_default();
        let enabled = config.enabled_providers.clone();
        let empty = BTreeMap::new();
        let provider_cfg = config.provider.as_ref().unwrap_or(&empty);

        // 1. catalog providers
        let mut providers: BTreeMap<String, Provider> = BTreeMap::new();
        for (id, cp) in catalog {
            let npm = cp
                .npm
                .clone()
                .unwrap_or_else(|| "@ai-sdk/openai-compatible".into());
            let base_url = cp
                .api
                .clone()
                .map(|u| expand_env(&u))
                .or_else(|| default_base_url(&npm, id).map(str::to_string))
                .unwrap_or_default();
            let will_connect = all_models
                || provider_cfg.contains_key(id)
                || creds.contains_key(id)
                || is_local(&base_url)
                || cp
                    .env
                    .iter()
                    .any(|v| std::env::var(v).map(|x| !x.is_empty()).unwrap_or(false));
            let models = if will_connect {
                cp.models
                    .iter()
                    .map(|(mid, m)| (mid.clone(), build_model(id, &npm, &base_url, mid, m)))
                    .collect()
            } else {
                BTreeMap::new()
            };
            providers.insert(
                id.clone(),
                Provider {
                    id: id.clone(),
                    name: if cp.name.is_empty() {
                        id.clone()
                    } else {
                        cp.name.clone()
                    },
                    npm,
                    base_url,
                    api_key: None,
                    headers: Vec::new(),
                    source: "catalog",
                    models,
                    reachable: false,
                },
            );
        }

        // 2. user config providers / overrides
        for (id, pc) in provider_cfg {
            let existing = providers.remove(id);
            let mut p = existing.unwrap_or_else(|| Provider {
                id: id.clone(),
                name: id.clone(),
                npm: "@ai-sdk/openai-compatible".into(),
                base_url: default_base_url("@ai-sdk/openai-compatible", id)
                    .unwrap_or_default()
                    .to_string(),
                api_key: None,
                headers: Vec::new(),
                source: "config",
                models: BTreeMap::new(),
                reachable: false,
            });
            apply_provider_config(&mut p, pc, id);
            p.source = "config";
            providers.insert(id.clone(), p);
        }

        // 3. env keys, 4. auth.json
        for (id, p) in providers.iter_mut() {
            if p.api_key.is_none()
                && let Some(cp) = catalog.get(id)
            {
                for var in &cp.env {
                    if let Ok(v) = std::env::var(var)
                        && !v.is_empty()
                    {
                        p.api_key = Some(v);
                        if p.source == "catalog" {
                            p.source = "env";
                        }
                        break;
                    }
                }
            }
            if let Some(AuthInfo::Api { key, .. }) = creds.get(id) {
                p.api_key = Some(key.clone());
                if p.source == "catalog" {
                    p.source = "auth";
                }
            }
        }

        // 5. local servers: only those actually listening count as connected
        for p in providers.values_mut() {
            if is_local(&p.base_url) {
                p.reachable = probe_local(&p.base_url);
            }
        }

        // 6. free-tier pool overlay + virtual `lunar` provider
        pool::apply(&mut providers, config, all_models);

        // 7. filters
        providers.retain(|id, _| !disabled.contains(id));
        if let Some(enabled) = enabled {
            providers.retain(|id, _| enabled.contains(id));
        }

        Self::from_providers(providers)
    }

    pub fn from_providers(providers: BTreeMap<String, Provider>) -> Self {
        Self {
            providers,
            client: crate::llm::LlmClient::new(),
        }
    }

    /// Providers usable right now (have a key, are local, or were configured).
    pub fn connected(&self) -> impl Iterator<Item = &Provider> {
        self.providers.values().filter(|p| p.connected())
    }

    pub fn get(&self, provider: &str, model: &str) -> Option<&Model> {
        self.providers.get(provider)?.models.get(model)
    }

    /// Parse `provider/model` (model ids may themselves contain `/`).
    pub fn parse_model_ref(s: &str) -> Option<(String, String)> {
        let (p, m) = s.split_once('/')?;
        if p.is_empty() || m.is_empty() {
            return None;
        }
        Some((p.to_string(), m.to_string()))
    }

    /// Resolve `provider/model` (or a bare model, searching connected providers).
    pub fn resolve(&self, spec: &str) -> Option<&Model> {
        if let Some((p, m)) = Self::parse_model_ref(spec)
            && let Some(model) = self.get(&p, &m)
        {
            return Some(model);
        }
        self.connected().find_map(|p| p.models.get(spec))
    }

    /// Pick a default model: config `model`, else the first connected provider's
    /// preferred model.
    pub fn default_model(&self, config: &Config) -> Option<&Model> {
        if let Some(spec) = &config.model
            && let Some(m) = self.resolve(spec)
        {
            return Some(m);
        }
        let preferred = [
            ("openai", "gpt-5"),
            ("openai", "gpt-4.1"),
            ("openai", "gpt-4o"),
            (pool::PROVIDER, "auto"),
            ("openrouter", "openai/gpt-4.1"),
            ("groq", "llama-3.3-70b-versatile"),
            ("deepseek", "deepseek-chat"),
        ];
        for (p, m) in preferred {
            if let Some(prov) = self.providers.get(p)
                && prov.connected()
                && let Some(model) = prov.models.get(m)
            {
                return Some(model);
            }
        }
        self.connected().find_map(|p| {
            p.models
                .values()
                .find(|m| m.tool_call && is_chat_model(&m.id))
                .or(p.models.values().find(|m| is_chat_model(&m.id)))
        })
    }

    pub fn small_model(&self, config: &Config, fallback: &Model) -> Model {
        if let Some(spec) = &config.small_model
            && let Some(m) = self.resolve(spec)
        {
            return m.clone();
        }
        if pool::is_virtual(fallback) {
            return fallback.clone();
        }
        // cheaper sibling from the same provider
        let candidates = [
            "gpt-4.1-mini",
            "gpt-4o-mini",
            "gpt-5-mini",
            "gpt-5-nano",
            "llama-3.1-8b-instant",
        ];
        if let Some(p) = self.providers.get(&fallback.provider_id) {
            for c in candidates {
                if let Some(m) = p.models.get(c) {
                    return m.clone();
                }
            }
        }
        fallback.clone()
    }

    /// Endpoint + protocol for a model.
    pub fn endpoint(&self, model: &Model) -> Result<(Arc<dyn Protocol>, Endpoint), String> {
        let provider = self.providers.get(&model.provider_id).ok_or("unknown provider")?;
        let protocol_id = model.protocol.ok_or_else(|| {
            format!(
                "provider package {} is not supported yet (only OpenAI-compatible providers)",
                model.npm
            )
        })?;
        let protocol = protocols::by_id(protocol_id).ok_or("no protocol")?;
        let mut endpoint = Endpoint::new(model.base_url.clone(), provider.api_key.clone());
        endpoint.headers.extend(provider.headers.iter().cloned());
        endpoint.headers.extend(model.headers.iter().cloned());
        if model.provider_id == "openrouter" {
            endpoint.headers.push((
                "HTTP-Referer".into(),
                "https://github.com/LunarLumos/LunarZero".into(),
            ));
            endpoint.headers.push(("X-Title".into(), "LunarZero".into()));
        }
        Ok((protocol, endpoint))
    }

    pub fn client(&self) -> &crate::llm::LlmClient {
        &self.client
    }

    pub fn to_response(&self, config: &Config) -> ProvidersResponse {
        let mut default = BTreeMap::new();
        let pool_cat = pool::catalog();
        let known = catalog::embedded();
        let providers = self
            .providers
            .values()
            // connected providers, plus every provider a user could connect
            // (free-tier pool members and catalog providers we can talk to)
            .filter(|p| {
                p.connected() || pool_cat.providers.contains_key(&p.id) || protocol_for(&p.npm).is_some()
            })
            .map(|p| {
                let connected = p.connected();
                if connected && let Some(m) = p.models.values().find(|m| m.protocol.is_some()) {
                    default.insert(p.id.clone(), m.id.clone());
                }
                let pool_p = pool_cat.providers.get(&p.id);
                ProviderInfo {
                    id: p.id.clone(),
                    name: p.name.clone(),
                    source: p.source.to_string(),
                    connected,
                    free: pool_p.is_some(),
                    signup: pool_p.map(|f| f.signup.clone()),
                    env: pool_p
                        .map(|f| f.env.clone())
                        .or_else(|| known.get(&p.id).map(|c| c.env.clone()))
                        .unwrap_or_default(),
                    local: is_local(&p.base_url),
                    models: p
                        .models
                        .values()
                        .filter(|m| connected && m.protocol.is_some() && is_chat_model(&m.id))
                        .map(|m| ModelInfo {
                            id: m.id.clone(),
                            provider_id: m.provider_id.clone(),
                            name: m.name.clone(),
                            family: m.family.clone(),
                            reasoning: m.reasoning,
                            tool_call: m.tool_call,
                            attachment: m.attachment,
                            temperature: m.temperature,
                            cost: m.cost,
                            limit: m.limit,
                            variants: m.variants.keys().cloned().collect(),
                            status: m.status.clone(),
                            pool: m.pool.clone(),
                        })
                        .collect(),
                }
            })
            .collect();
        if let Some(m) = self.default_model(config) {
            default.insert(m.provider_id.clone(), m.id.clone());
        }
        ProvidersResponse { providers, default }
    }
}

fn apply_provider_config(p: &mut Provider, pc: &ProviderConfig, id: &str) {
    if let Some(n) = &pc.name {
        p.name = n.clone();
    }
    if let Some(npm) = &pc.npm {
        p.npm = npm.clone();
    }
    if let Some(api) = &pc.api {
        p.base_url = expand_env(api);
    }
    if let Some(opts) = &pc.options {
        if let Some(u) = opts.get("baseURL").and_then(Value::as_str) {
            p.base_url = expand_env(u);
        }
        if let Some(k) = opts.get("apiKey").and_then(Value::as_str)
            && !k.is_empty()
        {
            p.api_key = Some(k.to_string());
        }
        if let Some(Value::Object(h)) = opts.get("headers") {
            for (k, v) in h {
                if let Some(v) = v.as_str() {
                    p.headers.push((k.clone(), v.to_string()));
                }
            }
        }
    }
    if p.base_url.is_empty() {
        p.base_url = default_base_url(&p.npm, id).unwrap_or_default().to_string();
    }
    // re-point existing models at the (possibly new) provider url/npm
    for m in p.models.values_mut() {
        if !m.url_overridden {
            m.base_url = p.base_url.clone();
        }
        if m.npm != p.npm && protocol_for(&m.npm).is_none() {
            m.npm = p.npm.clone();
            m.protocol = protocol_for(&p.npm);
        }
    }
    if let Some(models) = &pc.models {
        for (mid, mc) in models {
            let entry = p.models.entry(mid.clone()).or_insert_with(|| {
                build_model(
                    id,
                    &p.npm,
                    &p.base_url,
                    mid,
                    &CatalogModel {
                        tool_call: true,
                        temperature: true,
                        ..Default::default()
                    },
                )
            });
            apply_model_config(entry, mc);
        }
    }
    if let Some(wl) = &pc.whitelist {
        p.models.retain(|k, _| wl.contains(k));
    }
    if let Some(bl) = &pc.blacklist {
        p.models.retain(|k, _| !bl.contains(k));
    }
}

/// Convenience: full build from paths + config.
pub async fn load(paths: &Paths, config: &Config, refresh: bool) -> Registry {
    let catalog = catalog::load(&paths.models_cache(), refresh).await;
    let auth = AuthStore::new(paths);
    Registry::build(&catalog, config, &auth)
}

/// Like [`load`] but with every catalog model table populated.
pub async fn load_all(paths: &Paths, config: &Config, refresh: bool) -> Registry {
    let catalog = catalog::load(&paths.models_cache(), refresh).await;
    let auth = AuthStore::new(paths);
    Registry::build_with(&catalog, config, &auth, true)
}

pub fn provider_ids(catalog: &Catalog) -> Vec<&CatalogProvider> {
    catalog.values().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_openai_compatible_provider_from_config() {
        let catalog = catalog::embedded();
        let cfg: Config = serde_json::from_value(serde_json::json!({
            "provider": {
                "mylocal": {
                    "npm": "@ai-sdk/openai-compatible",
                    "options": { "baseURL": "http://localhost:8080/v1" },
                    "models": { "qwen": { "name": "Qwen", "limit": { "context": 32000, "output": 4096 } } }
                }
            }
        }))
        .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let auth = AuthStore::at(dir.path().join("auth.json"));
        let reg = Registry::build(&catalog, &cfg, &auth);
        let m = reg.get("mylocal", "qwen").unwrap();
        assert_eq!(m.base_url, "http://localhost:8080/v1");
        assert_eq!(m.protocol, Some("openai-chat"));
        assert!(reg.providers["mylocal"].connected());
        let (_, ep) = reg.endpoint(m).unwrap();
        assert_eq!(ep.base_url, "http://localhost:8080/v1");
        // catalog provider without key is not connected
        assert!(!reg.providers["openai"].connected());
        assert_eq!(reg.providers["openai"].base_url, "https://api.openai.com/v1");
        assert_eq!(reg.providers["groq"].base_url, "https://api.groq.com/openai/v1");
    }

    #[test]
    fn variants_from_reasoning_options() {
        let catalog = catalog::embedded();
        let dir = tempfile::tempdir().unwrap();
        let auth = AuthStore::at(dir.path().join("auth.json"));
        let reg = Registry::build_with(&catalog, &Config::default(), &auth, true);
        let m = reg.get("openai", "gpt-5").unwrap();
        // unconnected providers keep no model table by default
        let slim = Registry::build(&catalog, &Config::default(), &auth);
        assert!(slim.providers.contains_key("openai"));
        assert!(slim.providers["openai"].models.is_empty() || std::env::var("OPENAI_API_KEY").is_ok());
        assert!(m.variants.contains_key("high"));
        assert_eq!(m.variants["high"]["reasoningEffort"], "high");
    }
}
