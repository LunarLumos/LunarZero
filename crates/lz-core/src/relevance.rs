//! Tiny keyword relevance used to pick skills, MCP servers and routing
//! strategy from the user's prompt — no model call, microseconds, and
//! deterministic so the system prompt stays cache-friendly across a session.

use std::collections::HashSet;

const STOP: &[&str] = &[
    "the", "a", "an", "and", "or", "to", "of", "in", "on", "for", "with", "is", "it", "this", "that", "my",
    "me", "i", "you", "we", "be", "do", "can", "please", "help", "use", "using", "make", "how", "what",
    "why", "when", "some", "any", "all", "from", "into", "as", "at", "by", "if", "so", "not", "no", "yes",
    "then", "than", "also", "just", "like", "want", "need", "should", "would", "could", "will", "about",
    "there", "here", "have", "has", "had", "are", "was", "were", "am", "get", "got", "let", "us", "our",
    "your", "its", "up", "out", "one", "new", "add", "code", "file", "files", "project",
];

/// Lower-case word stems (plural/verb suffixes trimmed), stop words removed.
pub fn tokens(text: &str) -> HashSet<String> {
    text.split(|c: char| !c.is_alphanumeric() && c != '_' && c != '-' && c != '.')
        .flat_map(|w| w.split(['-', '_', '.']))
        .map(|w| w.trim().to_lowercase())
        .filter(|w| w.len() >= 3 && !STOP.contains(&w.as_str()))
        .map(|w| stem(&w))
        .collect()
}

pub fn stem(w: &str) -> String {
    for suf in ["ing", "ers", "ies", "ed", "es", "er", "s"] {
        if w.len() > suf.len() + 3 && w.ends_with(suf) {
            let base = &w[..w.len() - suf.len()];
            return if suf == "ies" {
                format!("{base}y")
            } else {
                base.to_string()
            };
        }
    }
    w.to_string()
}

/// Fraction of `keywords` present in `query` tokens, weighted so a single
/// strong hit on a short keyword list still counts.
pub fn score(query: &HashSet<String>, keywords: &str) -> f64 {
    let kw = tokens(keywords);
    if kw.is_empty() || query.is_empty() {
        return 0.0;
    }
    let hits = kw.iter().filter(|k| query.contains(*k)).count();
    if hits == 0 {
        return 0.0;
    }
    // hits matter more than coverage: 1 hit = 0.4, 2 = 0.65, 3+ ≈ 0.8+
    let by_hits = 1.0 - 0.6f64.powi(hits as i32);
    let coverage = hits as f64 / kw.len() as f64;
    0.7 * by_hits + 0.3 * coverage
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stems_and_scores() {
        let q = tokens("The tests are failing after my refactoring, fix the bug");
        assert!(q.contains("test"));
        assert!(q.contains("refactor"));
        assert!(q.contains("bug"));
        assert!(score(&q, "debug bug error crash failing") > 0.3);
        assert_eq!(score(&q, "postgres database sql"), 0.0);
    }
}
