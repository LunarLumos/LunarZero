//! Retry policy for provider calls.

use crate::llm::LlmError;

pub const RETRY_INITIAL_DELAY_MS: u64 = 2000;
pub const RETRY_BACKOFF_FACTOR: f64 = 2.0;
pub const RETRY_JITTER_FACTOR: f64 = 0.25;
pub const RETRY_MAX_DELAY_NO_HEADERS_MS: u64 = 30_000;
pub const RETRY_MAX_DELAY_MS: u64 = 2_147_483_647;
pub const RETRY_MAX_RETRIES: u32 = 5;

const RETRYABLE_PATTERNS: &[&str] = &[
    r"429|500|502|503|504|524",
    r"(?i)rate increased too quickly|rate limit|rate-limit|rate_limit|too many requests",
    r"(?i)overloaded|service unavailable|service_unavailable|service-unavailable|internal error|internal_error|internal server error|server error|server_error|server-error|provider returned error|provider_returned_error|provider-returned-error",
    r"(?i)terminated|fetch failed|failed to fetch|network[-_\s]error|upstream connect|connection error|connection refused|connection lost|socket connection was closed|socket hang up|reset before headers|getaddrinfo|enotfound|eai_again|econnrefused|econnreset|etimedout",
    r"(?i)^timeout$|\b(?:request|response|connection|network|stream|read) (?:timeout|timed out|time out)\b",
    r"(?i)try your request again|retry your request|resource exhausted|resource_exhausted",
    r"(?i)\btry again (?:later|in\b)|\b(?:currently|temporarily) at capacity\b",
];

fn matches_retryable_message(msg: &str) -> bool {
    RETRYABLE_PATTERNS
        .iter()
        .any(|p| regex::Regex::new(p).map(|r| r.is_match(msg)).unwrap_or(false))
}

/// Whether an error should be retried, with a user-facing message.
pub fn retryable(error: &LlmError) -> Option<String> {
    match error {
        LlmError::ContextOverflow { .. } | LlmError::Aborted | LlmError::Authentication { .. } => None,
        LlmError::InvalidRequest { .. } | LlmError::ContentPolicy { .. } | LlmError::InvalidOutput { .. } => {
            None
        }
        LlmError::RateLimited { message, .. } => Some(message.clone()),
        LlmError::Network { message } | LlmError::Timeout { message } => Some(message.clone()),
        LlmError::Provider {
            status,
            message,
            body,
            ..
        } => {
            if *status >= 500
                || error.retryable()
                || matches_retryable_message(message)
                || body.as_deref().is_some_and(matches_retryable_message)
            {
                Some(message.clone())
            } else {
                None
            }
        }
    }
}

fn exponential(attempt: u32, random: f64) -> u64 {
    let base = RETRY_INITIAL_DELAY_MS as f64 * RETRY_BACKOFF_FACTOR.powi(attempt.saturating_sub(1) as i32);
    (base + base * RETRY_JITTER_FACTOR * random).ceil() as u64
}

/// Delay before `attempt` (1-based). Honors `retry-after`; caps at 30s when the
/// provider gave no hint.
pub fn delay_ms(attempt: u32, error: Option<&LlmError>, random: f64) -> u64 {
    if let Some(ms) = error.and_then(LlmError::retry_after_ms) {
        return ms.min(RETRY_MAX_DELAY_MS);
    }
    let has_headers = matches!(error, Some(LlmError::Provider { headers, .. }) if !headers.is_empty());
    let exp = exponential(attempt, random);
    if has_headers {
        exp.min(RETRY_MAX_DELAY_MS)
    } else {
        exp.min(RETRY_MAX_DELAY_NO_HEADERS_MS)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delay_table() {
        assert_eq!(delay_ms(1, None, 0.0), 2000);
        assert_eq!(delay_ms(2, None, 0.0), 4000);
        assert_eq!(delay_ms(3, None, 0.0), 8000);
        assert_eq!(delay_ms(5, None, 0.0), 30_000); // capped (32000 > 30000)
        assert_eq!(delay_ms(1, None, 1.0), 2500); // jitter
        let e = LlmError::RateLimited {
            message: "slow".into(),
            retry_after_ms: Some(1234),
        };
        assert_eq!(delay_ms(1, Some(&e), 0.0), 1234);
    }

    #[test]
    fn retryable_classification() {
        assert!(
            retryable(&LlmError::RateLimited {
                message: "x".into(),
                retry_after_ms: None
            })
            .is_some()
        );
        assert!(retryable(&LlmError::ContextOverflow { message: "x".into() }).is_none());
        let e = LlmError::Provider {
            status: 400,
            message: "Bad request".into(),
            retry_after_ms: None,
            headers: Default::default(),
            body: None,
        };
        assert!(retryable(&e).is_none());
        let e = LlmError::Provider {
            status: 400,
            message: "the model is overloaded".into(),
            retry_after_ms: None,
            headers: Default::default(),
            body: None,
        };
        assert!(retryable(&e).is_some());
    }
}
