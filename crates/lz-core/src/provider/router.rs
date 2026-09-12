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

/// What the prompt asks for, derived from `user_text` (used by `auto`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Task {
    /// greetings, one-liners, quick lookups → fastest model
    Chat,
    /// edits, features, multi-file work with tools → high quality + tools
    Coding,
    /// "why / analyze / design / prove / compare" → prefer reasoning models
    Reasoning,
    /// big prompts → context-first
    LongContext,
}

pub fn task_of(need: &Need) -> Task {
    let t = need.user_text.to_lowercase();
    let words = |list: &[&str]| list.iter().any(|w| t.contains(w));
    if need.tokens > 24_000 {
        return Task::LongContext;
    }
    if words(&[
        "why",
        "analyze",
        "analyse",
        "design",
        "architect",
        "prove",
        "compare",
        "trade-off",
        "tradeoff",
        "plan ",
        "reason",
        "in depth",
        "root cause",
        "strategy",
        "evaluate",
    ]) {
        return Task::Reasoning;
    }
    if words(&[
        "implement",
        "refactor",
        "fix",
        "debug",
        "add ",
        "create",
        "write",
        "build",
        "migrate",
        "optimize",
        "test",
        "edit",
        "change",
        "update",
        "remove",
        "rename",
        "install",
        "deploy",
        "review",
    ]) || t.contains('`')
    {
        return Task::Coding;
    }
    if t.chars().count() < 160 {
        return Task::Chat;
    }
    Task::Coding
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
        let msg = compact_error(err);
        let lower = err.to_string().to_lowercase();
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
                let ms = if daily {
                    next_utc_midnight(now).saturating_sub(now)
                } else if let Some(ra) = retry_after_ms {
                    // the provider told us exactly when; trust it (+ a little slack)
                    (*ra).max(2_000) + 1_000
                } else {
                    (60_000 * (1u64 << u.failures.min(6))).min(60 * MINUTE)
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
                let too_large =
                    lower.contains("too large") || lower.contains("reduce your message") || *status == 413;
                let ms = match *status {
                    _ if too_large => 3 * MINUTE, // fine for smaller requests
                    404 => 24 * 60 * MINUTE,      // model id gone
                    400 | 422 => 30 * MINUTE,     // rejected request shape (often our schema)
                    402 | 403 => 6 * 60 * MINUTE, // billing / not entitled
                    429 => {
                        if daily {
                            next_utc_midnight(now).saturating_sub(now)
                        } else if let Some(ra) = retry_after_ms {
                            (*ra).max(2_000) + 1_000
                        } else {
                            (60_000 * (1u64 << u.failures.min(6))).min(60 * MINUTE)
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
    /// Why a model cannot be used right now, and when it can be (ms), if
    /// that is knowable. `None` wait = not until something else changes
    /// (needs a smaller request, a key, …).
    fn blocked(&self, l: &mut Ledger, model: &Model, tokens: u64, now: u64) -> Option<(String, Option<u64>)> {
        if let Some((until, _)) = l.providers.get(&model.provider_id)
            && *until > now
        {
            return Some(("provider cooling down".into(), Some(*until)));
        }
        let Some(free) = &model.pool else {
            return Some(("not in pool".into(), None));
        };
        let u = l.models.entry(Self::key(model)).or_default();
        u.prune(now);
        if u.cooldown_until > now {
            let why = if u.last_error.is_empty() {
                "cooling down".to_string()
            } else {
                format!(
                    "cooling down after: {}",
                    u.last_error.chars().take(60).collect::<String>()
                )
            };
            return Some((why, Some(u.cooldown_until)));
        }
        if let Some(rpm) = free.rpm {
            let used = u.count_since(now - MINUTE);
            if used >= rpm {
                let oldest = u.reqs.iter().rev().nth(rpm as usize - 1).copied().unwrap_or(now);
                return Some((format!("{rpm} requests/min used"), Some(oldest + MINUTE)));
            }
        }
        if let Some(rpd) = free.rpd {
            let used = u.count_since(now - DAY);
            if used >= rpd {
                let oldest = u.reqs.iter().rev().nth(rpd as usize - 1).copied().unwrap_or(now);
                return Some((format!("{rpd} requests/day used"), Some(oldest + DAY)));
            }
        }
        if let Some(tpm) = free.tpm {
            let used = u.tokens_since(now - MINUTE);
            if used + tokens > tpm {
                if tokens > tpm {
                    return Some((
                        format!(
                            "request ~{}k tokens > {}k tokens/min cap",
                            tokens / 1000,
                            tpm / 1000
                        ),
                        None,
                    ));
                }
                let first = u
                    .toks
                    .iter()
                    .rev()
                    .take_while(|(t, _)| *t >= now - MINUTE)
                    .last()
                    .map(|(t, _)| *t)
                    .unwrap_or(now);
                return Some((
                    format!("{}k of {}k tokens/min used", used / 1000, tpm / 1000),
                    Some(first + MINUTE),
                ));
            }
        }
        if let Some(tpd) = free.tpd {
            let used = u.tokens_since(now - DAY);
            if used + tokens > tpd {
                if tokens > tpd {
                    return Some((format!("request > {}k tokens/day cap", tpd / 1000), None));
                }
                let first = u
                    .toks
                    .iter()
                    .rev()
                    .take_while(|(t, _)| *t >= now - DAY)
                    .last()
                    .map(|(t, _)| *t)
                    .unwrap_or(now);
                return Some((
                    format!("{}k of {}k tokens/day used", used / 1000, tpd / 1000),
                    Some(first + DAY),
                ));
            }
        }
        None
    }

    fn available(&self, l: &mut Ledger, model: &Model, tokens: u64, now: u64) -> Result<f64, &'static str> {
        if self.blocked(l, model, tokens, now).is_some() {
            return Err("blocked");
        }
        let free = model.pool.as_ref().unwrap();
        let u = l.models.entry(Self::key(model)).or_default();
        let mut headroom: f64 = 1.0;
        if let Some(rpm) = free.rpm {
            headroom = headroom.min(1.0 - u.count_since(now - MINUTE) as f64 / rpm as f64);
        }
        if let Some(rpd) = free.rpd {
            headroom = headroom.min(1.0 - u.count_since(now - DAY) as f64 / rpd as f64);
        }
        if let Some(tpm) = free.tpm {
            headroom = headroom.min(1.0 - u.tokens_since(now - MINUTE) as f64 / tpm as f64);
        }
        if let Some(tpd) = free.tpd {
            headroom = headroom.min(1.0 - u.tokens_since(now - DAY) as f64 / tpd as f64);
        }
        Ok(headroom)
    }

    fn candidates<'a>(&self, registry: &'a Registry, need: &Need, exclude: &[String]) -> Vec<&'a Model> {
        registry
            .connected()
            .filter(|p| p.id != pool::PROVIDER)
            .flat_map(|p| p.models.values())
            .filter(|m| m.pool.is_some() && m.protocol.is_some())
            .filter(|m| !exclude.contains(&Self::key(m)))
            .filter(|m| !need.tools || m.tool_call)
            .filter(|m| !need.vision || m.attachment)
            .filter(|m| m.limit.context >= (need.tokens as f64) * 1.1 + 2048.0)
            .collect()
    }

    /// Every pool model that fits the request but is blocked right now, with
    /// the reason and (when known) how long until it frees up.
    pub fn explain(
        &self,
        registry: &Registry,
        need: &Need,
        exclude: &[String],
    ) -> Vec<(String, String, Option<u64>)> {
        let now = now_ms();
        let mut l = self.ledger.lock().unwrap();
        let mut out = Vec::new();
        for m in self.candidates(registry, need, exclude) {
            if let Some((why, until)) = self.blocked(&mut l, m, need.tokens, now) {
                out.push((Self::key(m), why, until.map(|u| u.saturating_sub(now))));
            }
        }
        out.sort_by_key(|(_, _, w)| w.unwrap_or(u64::MAX));
        out
    }

    /// The blocked model that frees up soonest (model, wait ms, reason), if
    /// any will within `max_wait_ms`.
    pub fn soonest(
        &self,
        registry: &Registry,
        need: &Need,
        exclude: &[String],
        max_wait_ms: u64,
    ) -> Option<(Model, u64, String)> {
        let now = now_ms();
        let mut l = self.ledger.lock().unwrap();
        let mut best: Option<(Model, u64, String)> = None;
        for m in self.candidates(registry, need, exclude) {
            if let Some((why, Some(until))) = self.blocked(&mut l, m, need.tokens, now) {
                let wait = until.saturating_sub(now);
                if wait <= max_wait_ms && best.as_ref().is_none_or(|(_, w, _)| wait < *w) {
                    best = Some((m.clone(), wait, why));
                }
            }
        }
        best
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
        let candidates: Vec<&Model> = self.candidates(registry, need, exclude);
        if candidates.is_empty() {
            return None;
        }
        // sticky: same model for a session while it stays usable — unless a
        // clearly better model (≥ 12 quality points) is free now, e.g. after a
        // stronger provider's key was added or its cooldown ended
        if sticky_minutes > 0 {
            let sticky = self.sticky.lock().unwrap();
            if let Some((key, until)) = sticky.get(session_id)
                && *until > now
                && let Some(m) = candidates.iter().find(|m| &Self::key(m) == key)
                && self.available(&mut l, m, need.tokens, now).is_ok()
            {
                let mine = m.pool.as_ref().map(|p| p.quality).unwrap_or(0);
                let best_free = candidates
                    .iter()
                    .filter(|c| self.available(&mut l, c, need.tokens, now).is_ok())
                    .map(|c| c.pool.as_ref().map(|p| p.quality).unwrap_or(0))
                    .max()
                    .unwrap_or(mine);
                if best_free < mine + 12 {
                    return Some(Pick {
                        model: (*m).clone(),
                        reason: "sticky".into(),
                    });
                }
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
            // task-specific nudges: thinking models for reasoning work, big
            // windows for long prompts, nothing slow for a quick chat
            let nudge = match task_of(need) {
                Task::Reasoning if m.reasoning => 0.10,
                Task::Chat if m.reasoning => -0.05,
                Task::LongContext => (m.limit.context / 1_048_576.0).min(1.0) * 0.10,
                _ => 0.0,
            };
            let score = w_int * intelligence + w_speed * speed + w_head * headroom - 0.05 * failures + nudge;
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
        let task = match task_of(need) {
            Task::Chat => "chat",
            Task::Coding => "coding",
            Task::Reasoning => "reasoning",
            Task::LongContext => "long-context",
        };
        Some(Pick {
            model: m.clone(),
            reason: format!("{} for {task}", effective.name()),
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

/// Short, human error text for ledgers and status lines (JSON bodies → their message).
pub fn compact_error(err: &LlmError) -> String {
    let raw = err.to_string();
    let body = raw.split_once(": ").map(|(_, b)| b).unwrap_or(&raw);
    let text = lz_schema::session::json_error_message(body).unwrap_or_else(|| body.to_string());
    let mut one: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    for marker in [
        " For more information",
        " To monitor",
        " Learn more",
        " See https://",
    ] {
        if let Some(i) = one.find(marker) {
            one.truncate(i);
        }
    }
    let status = match err {
        LlmError::Provider { status, .. } => format!("HTTP {status}: "),
        LlmError::RateLimited { .. } => "rate limited: ".into(),
        LlmError::Timeout { .. } => "timeout: ".into(),
        LlmError::Network { .. } => "network: ".into(),
        _ => String::new(),
    };
    let mut s = format!("{status}{one}");
    if s.chars().count() > 120 {
        s = s.chars().take(119).collect::<String>() + "…";
    }
    s
}

/// The `auto` strategy: agentic/coding work with tools or big prompts wants
/// the smartest model; short chat wants the fastest; everything else balances.
fn classify(need: &Need) -> Strategy {
    match task_of(need) {
        Task::Chat => Strategy::Fast,
        Task::Coding | Task::Reasoning | Task::LongContext => Strategy::Smart,
    }
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
                    reachable: false,
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

    fn need() -> Need {
        Need {
            tools: true,
            ..Default::default()
        }
    }

    #[test]
    fn retry_after_is_honoured_exactly() {
        let reg = registry(vec![model("a", "m", 80, 50, None)]);
        let r = Router::new(None);
        let m = reg.get("a", "m").unwrap().clone();
        let d = r.record_failure(
            &m,
            &LlmError::RateLimited {
                message: "slow down".into(),
                retry_after_ms: Some(4_000),
            },
        );
        // provider said 4 s: wait that plus a second of slack, not a backoff table
        assert_eq!(d, Duration::from_millis(5_000));
        assert!(r.pick(&reg, Strategy::Auto, &need(), "s", &[], 0).is_none());
        let (_, wait, why) = r.soonest(&reg, &need(), &[], 120_000).unwrap();
        assert!((4_000..=5_000).contains(&wait), "{wait}");
        assert!(!why.is_empty());
        // and not for a wait longer than the caller accepts
        assert!(r.soonest(&reg, &need(), &[], 1_000).is_none());
    }

    #[test]
    fn daily_quota_cools_down_until_utc_midnight() {
        let reg = registry(vec![model("a", "m", 80, 50, None)]);
        let r = Router::new(None);
        let m = reg.get("a", "m").unwrap().clone();
        let d = r.record_failure(
            &m,
            &LlmError::RateLimited {
                message: "Rate limit reached: requests per day (RPD) exhausted".into(),
                retry_after_ms: Some(2_000),
            },
        );
        // a daily cap ignores retry-after: it is out until the day rolls over
        assert!(d > Duration::from_secs(60), "{d:?}");
        assert!(d <= Duration::from_secs(24 * 3600));
        let explain = r.explain(&reg, &need(), &[]);
        assert_eq!(explain.len(), 1);
        assert!(explain[0].2.unwrap() > 60_000);
    }

    #[test]
    fn auth_failure_benches_the_whole_provider() {
        let reg = registry(vec![
            model("a", "m1", 80, 50, None),
            model("a", "m2", 70, 60, None),
            model("b", "other", 60, 70, None),
        ]);
        let r = Router::new(None);
        let m1 = reg.get("a", "m1").unwrap().clone();
        r.record_failure(
            &m1,
            &LlmError::Authentication {
                message: "invalid api key".into(),
            },
        );
        // every model of provider `a` is blocked, `b` still serves
        let pick = r.pick(&reg, Strategy::Auto, &need(), "s", &[], 0).unwrap();
        assert_eq!(pick.model.provider_id, "b");
        let blocked: Vec<String> = r
            .explain(&reg, &need(), &[])
            .into_iter()
            .map(|(k, _, _)| k)
            .collect();
        assert!(blocked.contains(&"a/m1".to_string()) && blocked.contains(&"a/m2".to_string()));
    }

    #[test]
    fn missing_model_is_out_for_a_day_and_backoff_grows() {
        let reg = registry(vec![model("a", "m", 80, 50, None)]);
        let r = Router::new(None);
        let m = reg.get("a", "m").unwrap().clone();
        let gone = LlmError::Provider {
            status: 404,
            message: "model not found".into(),
            retry_after_ms: None,
            headers: Default::default(),
            body: None,
        };
        assert_eq!(r.record_failure(&m, &gone), Duration::from_secs(24 * 3600));

        let r2 = Router::new(None);
        let flaky = LlmError::Provider {
            status: 503,
            message: "unavailable".into(),
            retry_after_ms: None,
            headers: Default::default(),
            body: None,
        };
        let first = r2.record_failure(&m, &flaky);
        let second = r2.record_failure(&m, &flaky);
        let third = r2.record_failure(&m, &flaky);
        assert!(first < second && second < third, "{first:?} {second:?} {third:?}");
        // …but capped, so a flapping provider is retried within minutes, not hours
        for _ in 0..20 {
            r2.record_failure(&m, &flaky);
        }
        assert!(r2.record_failure(&m, &flaky) <= Duration::from_secs(15 * 60));
    }

    #[test]
    fn tried_models_are_skipped_and_exhaustion_is_explained() {
        let reg = registry(vec![
            model("a", "m1", 80, 50, None),
            model("b", "m2", 70, 60, None),
        ]);
        let r = Router::new(None);
        // the step already tried m1 (failed mid-stream, say): the next pick is m2
        let pick = r
            .pick(&reg, Strategy::Auto, &need(), "s", &["a/m1".to_string()], 0)
            .unwrap();
        assert_eq!(pick.model.id, "m2");
        // both tried → nothing, and nothing is "soonest" either: the caller must stop, not spin
        let tried = vec!["a/m1".to_string(), "b/m2".to_string()];
        assert!(r.pick(&reg, Strategy::Auto, &need(), "s", &tried, 0).is_none());
        assert!(r.soonest(&reg, &need(), &tried, u64::MAX).is_none());
        assert!(r.explain(&reg, &need(), &tried).is_empty());
    }

    #[test]
    fn success_clears_failure_streak() {
        let reg = registry(vec![model("a", "m", 80, 50, None)]);
        let r = Router::new(None);
        let m = reg.get("a", "m").unwrap().clone();
        let flaky = LlmError::Provider {
            status: 500,
            message: "boom".into(),
            retry_after_ms: None,
            headers: Default::default(),
            body: None,
        };
        r.record_failure(&m, &flaky);
        r.record_failure(&m, &flaky);
        r.reset_cooldowns();
        assert!(r.pick(&reg, Strategy::Auto, &need(), "s", &[], 0).is_some());
        // a good request afterwards keeps it usable
        r.record_request(&m, 500);
        r.record_latency(&m, 300, 100, 1_000);
        assert!(r.pick(&reg, Strategy::Auto, &need(), "s", &[], 0).is_some());
    }

    #[test]
    fn sticky_keeps_session_on_model() {
        // two models of similar quality: the session sticks to its first pick
        let reg = registry(vec![
            model("a", "big", 72, 20, None),
            model("b", "quick", 62, 95, None),
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
        // strategy changes but sticky wins while the model is usable and no
        // clearly better model is free
        assert_eq!(
            r.pick(&reg, Strategy::Smart, &need, "s", &[], 30).unwrap().reason,
            "sticky"
        );
        // a much better model appearing breaks stickiness
        let reg2 = registry(vec![
            model("a", "big", 95, 20, None),
            model("b", "quick", 62, 95, None),
        ]);
        assert_eq!(
            r.pick(&reg2, Strategy::Smart, &need, "s", &[], 30)
                .unwrap()
                .model
                .id,
            "big"
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
