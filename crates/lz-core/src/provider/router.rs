//! Pool router: picks a concrete model for `lunar/*`, keeps per-model usage
//! windows (RPM/RPD/TPM/TPD), cooldowns and measured latency, and fails over
//! on rate limits / outages. State survives restarts via `state/quota.json`.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::pool::{self, Strategy};
use super::{Model, Registry};
use crate::llm::LlmError;

const MINUTE: u64 = 60_000;
const DAY: u64 = 86_400_000;

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn next_utc_midnight(now: u64) -> u64 {
    (now / DAY + 1) * DAY
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct Usage {
    /// request timestamps (ms), last 24h
    reqs: VecDeque<u64>,
    /// (timestamp, tokens), last 24h
    toks: VecDeque<(u64, u64)>,
    /// cooldown until (ms) and how many consecutive failures led to it
    #[serde(default)]
    cooldown_until: u64,
    #[serde(default)]
    failures: u32,
    #[serde(default)]
    last_error: String,
    /// measured time-to-first-token (ms, EMA; 0 = unknown)
    #[serde(default)]
    ttft_ms: f64,
    /// measured output tokens per second (EMA; 0 = unknown)
    #[serde(default)]
    tps: f64,
}

impl Usage {
    fn prune(&mut self, now: u64) {
        while self.reqs.front().is_some_and(|t| *t + DAY < now) {
            self.reqs.pop_front();
        }
        while self.toks.front().is_some_and(|(t, _)| *t + DAY < now) {
            self.toks.pop_front();
        }
    }
    fn count_since(&self, since: u64) -> u64 {
        self.reqs.iter().rev().take_while(|t| **t >= since).count() as u64
    }
    fn tokens_since(&self, since: u64) -> u64 {
        self.toks
            .iter()
            .rev()
            .take_while(|(t, _)| *t >= since)
            .map(|(_, n)| *n)
            .sum()
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Ledger {
    /// `provider/model` → usage
    models: HashMap<String, Usage>,
    /// provider → cooldown until (auth failures, provider-wide outages)
    providers: HashMap<String, (u64, String)>,
}

/// What the request needs from a model.
#[derive(Debug, Clone, Default)]
pub struct Need {
    pub tools: bool,
    pub vision: bool,
    /// estimated prompt tokens
    pub tokens: u64,
    /// last user message, for the auto strategy's task heuristic
    pub user_text: String,
}

#[derive(Debug, Clone)]
pub struct Pick {
    pub model: Model,
    pub reason: String,
}

/// Per-model status for `lz pool status` / the TUI.
#[derive(Debug, Clone, Serialize)]
pub struct ModelUsage {
    pub provider: String,
    pub model: String,
    pub rpm_used: u64,
    pub rpd_used: u64,
    pub tpm_used: u64,
    pub tpd_used: u64,
    pub cooldown_secs: u64,
    pub last_error: String,
    pub ttft_ms: u64,
    pub tps: u64,
}

pub struct Router {
    ledger: Mutex<Ledger>,
    path: Option<PathBuf>,
    /// session → (provider/model, expires at ms)
    sticky: Mutex<HashMap<String, (String, u64)>>,
}

impl Router {
    pub fn new(path: Option<PathBuf>) -> Router {
        let ledger = path
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str::<Ledger>(&s).ok())
            .unwrap_or_default();
        Router {
            ledger: Mutex::new(ledger),
            path,
            sticky: Mutex::new(HashMap::new()),
        }
    }

    fn save(&self, ledger: &Ledger) {
        let Some(path) = &self.path else { return };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(s) = serde_json::to_string(ledger) {
            let tmp = path.with_extension("json.tmp");
            if std::fs::write(&tmp, s).is_ok() {
                let _ = std::fs::rename(&tmp, path);
            }
        }
    }

    fn key(model: &Model) -> String {
        format!("{}/{}", model.provider_id, model.id)
    }

    /// Record a completed (or started) request so the windows stay honest.
    pub fn record_request(&self, model: &Model, tokens: u64) {
        let now = now_ms();
        let mut l = self.ledger.lock().unwrap();
        let u = l.models.entry(Self::key(model)).or_default();
        u.prune(now);
        u.reqs.push_back(now);
        if tokens > 0 {
            u.toks.push_back((now, tokens));
        }
        u.failures = 0;
        self.save(&l);
    }

    /// Add token usage learned after the response finished.
    pub fn record_tokens(&self, model: &Model, tokens: u64) {
        if tokens == 0 {
            return;
        }
        let now = now_ms();
        let mut l = self.ledger.lock().unwrap();
        let u = l.models.entry(Self::key(model)).or_default();
        u.toks.push_back((now, tokens));
        self.save(&l);
    }

    /// Fold a measured response into the model's latency profile.
    pub fn record_latency(&self, model: &Model, ttft_ms: u64, output_tokens: u64, gen_ms: u64) {
        let mut l = self.ledger.lock().unwrap();
        let u = l.models.entry(Self::key(model)).or_default();
        let ema = |old: f64, new: f64| if old <= 0.0 { new } else { old * 0.7 + new * 0.3 };
        if ttft_ms > 0 {
            u.ttft_ms = ema(u.ttft_ms, ttft_ms as f64);
        }
        if output_tokens >= 20 && gen_ms > 0 {
            u.tps = ema(u.tps, output_tokens as f64 * 1000.0 / gen_ms as f64);
        }
        self.save(&l);
    }

    /// Measured speed 0–1 (None when nothing was measured yet).
    fn measured_speed(u: &Usage) -> Option<f64> {
        if u.ttft_ms <= 0.0 && u.tps <= 0.0 {
            return None;
        }
        let ttft = if u.ttft_ms > 0.0 {
            (1.0 - (u.ttft_ms - 300.0) / 4700.0).clamp(0.0, 1.0)
        } else {
            0.5
        };
        let tps = if u.tps > 0.0 {
            ((u.tps - 10.0) / 140.0).clamp(0.0, 1.0)
        } else {
            0.5
        };
        Some(0.5 * ttft + 0.5 * tps)
    }

    /// Cool a model (or its whole provider) down after a failure. Returns the
    /// cooldown applied, for logging.
    pub fn record_failure(&self, model: &Model, err: &LlmError) -> Duration {
        let now = now_ms();
        let msg = err.to_string();
        let lower = msg.to_lowercase();
        let daily = lower.contains("per day")
            || lower.contains("daily")
            || lower.contains("quota exceeded")
            || lower.contains("rpd")
            || lower.contains("tokens per day");
        let mut l = self.ledger.lock().unwrap();
        let cooldown = match err {
            LlmError::Authentication { .. } => {
                l.providers
                    .insert(model.provider_id.clone(), (now + 60 * MINUTE, msg.clone()));
                Duration::from_secs(3600)
            }
            LlmError::RateLimited { retry_after_ms, .. } => {
                let u = l.models.entry(Self::key(model)).or_default();
                u.failures += 1;
                let base = retry_after_ms.unwrap_or(0).max(60_000);
                let ms = if daily {
                    next_utc_midnight(now).saturating_sub(now)
                } else {
                    (base * (1u64 << u.failures.min(6))).min(60 * MINUTE)
                };
                u.cooldown_until = now + ms;
                u.last_error = msg.clone();
                Duration::from_millis(ms)
            }
            LlmError::Provider {
                status,
                retry_after_ms,
                ..
            } => {
                let u = l.models.entry(Self::key(model)).or_default();
                u.failures += 1;
                let ms = match *status {
                    404 => 24 * 60 * MINUTE,      // model id gone
                    400 | 422 => 30 * MINUTE,     // rejected request shape (often our schema)
                    402 | 403 => 6 * 60 * MINUTE, // billing / not entitled
                    429 => {
                        if daily {
                            next_utc_midnight(now).saturating_sub(now)
                        } else {
                            (retry_after_ms.unwrap_or(60_000).max(60_000) * (1u64 << u.failures.min(6)))
                                .min(60 * MINUTE)
                        }
                    }
                    _ => (30_000 * (1u64 << u.failures.min(5))).min(15 * MINUTE),
                };
                u.cooldown_until = now + ms;
                u.last_error = msg.clone();
                Duration::from_millis(ms)
            }
            LlmError::Network { .. } | LlmError::Timeout { .. } => {
                let u = l.models.entry(Self::key(model)).or_default();
                u.failures += 1;
                let ms = (30_000 * (1u64 << u.failures.min(4))).min(10 * MINUTE);
                u.cooldown_until = now + ms;
                u.last_error = msg.clone();
                Duration::from_millis(ms)
            }
            LlmError::InvalidOutput { .. } | LlmError::InvalidRequest { .. } => {
                let u = l.models.entry(Self::key(model)).or_default();
                u.failures += 1;
                let ms = 10 * MINUTE;
                u.cooldown_until = now + ms;
                u.last_error = msg.clone();
                Duration::from_millis(ms)
            }
            _ => Duration::ZERO,
        };
        self.save(&l);
        cooldown
    }

    /// Whether a model is currently usable: not cooling down and under every
    /// limit it declares. `tokens` is the estimated prompt size.
    fn available(&self, l: &mut Ledger, model: &Model, tokens: u64, now: u64) -> Result<f64, &'static str> {
        if let Some((until, _)) = l.providers.get(&model.provider_id)
            && *until > now
        {
            return Err("provider cooldown");
        }
        let Some(free) = &model.pool else {
            return Err("not in pool");
        };
        let u = l.models.entry(Self::key(model)).or_default();
        u.prune(now);
        if u.cooldown_until > now {
            return Err("cooldown");
        }
        // headroom = smallest remaining fraction across declared limits
        let mut headroom: f64 = 1.0;
        if let Some(rpm) = free.rpm {
            let used = u.count_since(now - MINUTE);
            if used >= rpm {
                return Err("rpm");
            }
            headroom = headroom.min(1.0 - used as f64 / rpm as f64);
        }
        if let Some(rpd) = free.rpd {
            let used = u.count_since(now - DAY);
            if used >= rpd {
                return Err("rpd");
            }
            headroom = headroom.min(1.0 - used as f64 / rpd as f64);
        }
        if let Some(tpm) = free.tpm {
            let used = u.tokens_since(now - MINUTE);
            if used + tokens > tpm {
                return Err("tpm");
            }
            headroom = headroom.min(1.0 - used as f64 / tpm as f64);
        }
        if let Some(tpd) = free.tpd {
            let used = u.tokens_since(now - DAY);
            if used + tokens > tpd {
                return Err("tpd");
            }
            headroom = headroom.min(1.0 - used as f64 / tpd as f64);
        }
        Ok(headroom)
    }

    /// Pick the best pool model for `need`, skipping `exclude` (`provider/model` keys).
    pub fn pick(
        &self,
        registry: &Registry,
        strategy: Strategy,
        need: &Need,
        session_id: &str,
        exclude: &[String],
        sticky_minutes: u64,
    ) -> Option<Pick> {
        let now = now_ms();
        let mut l = self.ledger.lock().unwrap();
        let candidates: Vec<&Model> = registry
            .connected()
            .filter(|p| p.id != pool::PROVIDER)
            .flat_map(|p| p.models.values())
            .filter(|m| m.pool.is_some() && m.protocol.is_some())
            .filter(|m| !exclude.contains(&Self::key(m)))
            .filter(|m| !need.tools || m.tool_call)
            .filter(|m| !need.vision || m.attachment)
            .filter(|m| m.limit.context >= (need.tokens as f64) * 1.1 + 2048.0)
            .collect();
        if candidates.is_empty() {
            return None;
        }
        // sticky: same model for a session while it stays usable
        if sticky_minutes > 0 {
            let sticky = self.sticky.lock().unwrap();
            if let Some((key, until)) = sticky.get(session_id)
                && *until > now
                && let Some(m) = candidates.iter().find(|m| &Self::key(m) == key)
                && self.available(&mut l, m, need.tokens, now).is_ok()
            {
                return Some(Pick {
                    model: (*m).clone(),
                    reason: "sticky".into(),
                });
            }
        }
        let effective = match strategy {
            Strategy::Auto => classify(need),
            s => s,
        };
        let (w_int, w_speed, w_head) = match effective {
            Strategy::Smart => (0.75, 0.05, 0.20),
            Strategy::Fast => (0.25, 0.55, 0.20),
            Strategy::Auto => (0.50, 0.30, 0.20),
        };
        let mut best: Option<(f64, &Model)> = None;
        for m in &candidates {
            let Ok(headroom) = self.available(&mut l, m, need.tokens, now) else {
                continue;
            };
            let f = m.pool.as_ref().unwrap();
            let intelligence = f.quality as f64 / 100.0;
            let usage = l.models.get(&Self::key(m));
            // static prior, replaced half-and-half by what we actually measured
            let speed = match usage.and_then(Self::measured_speed) {
                Some(measured) => 0.5 * f.speed as f64 / 100.0 + 0.5 * measured,
                None => f.speed as f64 / 100.0,
            };
            let failures = usage.map(|u| u.failures).unwrap_or(0) as f64;
            let score = w_int * intelligence + w_speed * speed + w_head * headroom - 0.05 * failures;
            if best.is_none_or(|(s, _)| score > s) {
                best = Some((score, m));
            }
        }
        let (_, m) = best?;
        let key = Self::key(m);
        if sticky_minutes > 0 {
            self.sticky
                .lock()
                .unwrap()
                .insert(session_id.to_string(), (key, now + sticky_minutes * MINUTE));
        }
        Some(Pick {
            model: m.clone(),
            reason: effective.name().into(),
        })
    }

    /// Drop every cooldown (usage windows are kept so caps stay honest).
    pub fn reset_cooldowns(&self) {
        let mut l = self.ledger.lock().unwrap();
        l.providers.clear();
        for u in l.models.values_mut() {
            u.cooldown_until = 0;
            u.failures = 0;
            u.last_error.clear();
        }
        self.save(&l);
    }

    pub fn clear_sticky(&self, session_id: &str) {
        self.sticky.lock().unwrap().remove(session_id);
    }

    /// Snapshot for status displays.
    pub fn usage(&self, registry: &Registry) -> Vec<ModelUsage> {
        let now = now_ms();
        let mut l = self.ledger.lock().unwrap();
        let mut out = Vec::new();
        for p in registry.providers.values() {
            if p.id == pool::PROVIDER {
                continue;
            }
            for m in p.models.values().filter(|m| m.pool.is_some()) {
                let key = Self::key(m);
                let provider_cd = l.providers.get(&p.id).map(|(u, _)| *u).unwrap_or(0);
                let u = l.models.entry(key).or_default();
                u.prune(now);
                let cd = u.cooldown_until.max(provider_cd).saturating_sub(now) / 1000;
                out.push(ModelUsage {
                    provider: p.id.clone(),
                    model: m.id.clone(),
                    rpm_used: u.count_since(now - MINUTE),
                    rpd_used: u.count_since(now - DAY),
                    tpm_used: u.tokens_since(now - MINUTE),
                    tpd_used: u.tokens_since(now - DAY),
                    cooldown_secs: cd,
                    last_error: if cd > 0 {
                        u.last_error.clone()
                    } else {
                        String::new()
                    },
                    ttft_ms: u.ttft_ms as u64,
                    tps: u.tps as u64,
                });
            }
        }
        out
    }
}

/// The `auto` strategy: agentic/coding work with tools or big prompts wants
/// the smartest model; short chat wants the fastest; everything else balances.
fn classify(need: &Need) -> Strategy {
    let text = need.user_text.to_lowercase();
    let long = need.tokens > 24_000;
    let quick_words = [
        "quick",
        "briefly",
        "one word",
        "yes or no",
        "tl;dr",
        "short answer",
    ];
    let hard_words = [
        "refactor",
        "implement",
        "debug",
        "architecture",
        "design",
        "prove",
        "analyze",
        "analyse",
        "migrate",
        "optimize",
        "security",
        "review",
    ];
    if need.tools || long || hard_words.iter().any(|w| text.contains(w)) {
        return Strategy::Smart;
    }
    if text.chars().count() < 160 || quick_words.iter().any(|w| text.contains(w)) {
        return Strategy::Fast;
    }
    Strategy::Auto
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(p: &str, id: &str, quality: u32, speed: u32, rpm: Option<u64>) -> Model {
        Model {
            provider_id: p.into(),
            id: id.into(),
            api_id: id.into(),
            name: id.into(),
            family: None,
            npm: "@ai-sdk/openai-compatible".into(),
            base_url: "http://x".into(),
            reasoning: false,
            tool_call: true,
            attachment: false,
            temperature: true,
            cost: Default::default(),
            limit: lz_schema::api::ModelLimit {
                context: 128_000.0,
                input: None,
                output: 8192.0,
            },
            status: None,
            variants: Default::default(),
            options: Default::default(),
            headers: Vec::new(),
            protocol: Some("openai-chat"),
            url_overridden: false,
            pool: Some(lz_schema::api::PoolInfo {
                quality,
                speed,
                rpm,
                ..Default::default()
            }),
        }
    }

    fn registry(models: Vec<Model>) -> Registry {
        let mut providers = std::collections::BTreeMap::new();
        for m in models {
            let p = providers
                .entry(m.provider_id.clone())
                .or_insert_with(|| super::super::Provider {
                    id: m.provider_id.clone(),
                    name: m.provider_id.clone(),
                    npm: "@ai-sdk/openai-compatible".into(),
                    base_url: "http://x".into(),
                    api_key: Some("k".into()),
                    headers: Vec::new(),
                    source: "env",
                    models: Default::default(),
                });
            p.models.insert(m.id.clone(), m);
        }
        Registry::from_providers(providers)
    }

    #[test]
    fn smart_prefers_quality_fast_prefers_speed() {
        let reg = registry(vec![
            model("a", "big", 90, 20, None),
            model("b", "quick", 30, 95, None),
        ]);
        let r = Router::new(None);
        let need = Need {
            tools: true,
            ..Default::default()
        };
        assert_eq!(
            r.pick(&reg, Strategy::Smart, &need, "s", &[], 0)
                .unwrap()
                .model
                .id,
            "big"
        );
        assert_eq!(
            r.pick(&reg, Strategy::Fast, &need, "s", &[], 0).unwrap().model.id,
            "quick"
        );
    }

    #[test]
    fn rate_limit_and_cooldown_fail_over() {
        let reg = registry(vec![
            model("a", "big", 90, 20, Some(1)),
            model("b", "quick", 30, 95, None),
        ]);
        let r = Router::new(None);
        let need = Need {
            tools: true,
            ..Default::default()
        };
        let first = r.pick(&reg, Strategy::Smart, &need, "s", &[], 0).unwrap().model;
        assert_eq!(first.id, "big");
        r.record_request(&first, 100);
        // rpm exhausted → next best
        assert_eq!(
            r.pick(&reg, Strategy::Smart, &need, "s", &[], 0)
                .unwrap()
                .model
                .id,
            "quick"
        );
        // explicit 429 on quick → nothing left
        let quick = reg.get("b", "quick").unwrap().clone();
        r.record_failure(
            &quick,
            &LlmError::RateLimited {
                message: "slow down".into(),
                retry_after_ms: None,
            },
        );
        assert!(r.pick(&reg, Strategy::Smart, &need, "s", &[], 0).is_none());
        let usage = r.usage(&reg);
        assert!(usage.iter().any(|u| u.model == "quick" && u.cooldown_secs > 0));
    }

    #[test]
    fn sticky_keeps_session_on_model() {
        let reg = registry(vec![
            model("a", "big", 90, 20, None),
            model("b", "quick", 30, 95, None),
        ]);
        let r = Router::new(None);
        let need = Need::default();
        assert_eq!(
            r.pick(&reg, Strategy::Fast, &need, "s", &[], 30)
                .unwrap()
                .model
                .id,
            "quick"
        );
        // strategy changes but sticky wins while the model is usable
        assert_eq!(
            r.pick(&reg, Strategy::Smart, &need, "s", &[], 30).unwrap().reason,
            "sticky"
        );
        r.clear_sticky("s");
        assert_eq!(
            r.pick(&reg, Strategy::Smart, &need, "s", &[], 30)
                .unwrap()
                .model
                .id,
            "big"
        );
    }

    #[test]
    fn tools_requirement_filters() {
        let mut no_tools = model("a", "chat", 95, 95, None);
        no_tools.tool_call = false;
        let reg = registry(vec![no_tools, model("b", "agent", 60, 60, None)]);
        let r = Router::new(None);
        let need = Need {
            tools: true,
            ..Default::default()
        };
        assert_eq!(
            r.pick(&reg, Strategy::Smart, &need, "s", &[], 0)
                .unwrap()
                .model
                .id,
            "agent"
        );
    }
}
