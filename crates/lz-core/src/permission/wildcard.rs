//! Glob-ish matcher used by permission rules: `*` → `.*`, `?` → `.`,
//! a trailing `" *"` is optional (so `git *` also matches bare `git`).

use std::collections::HashMap;
use std::sync::Mutex;

use regex::Regex;

static CACHE: Mutex<Option<HashMap<String, Regex>>> = Mutex::new(None);

fn compile(pattern: &str) -> Regex {
    let mut escaped = String::with_capacity(pattern.len() * 2);
    for ch in pattern.replace('\\', "/").chars() {
        match ch {
            '*' => escaped.push_str(".*"),
            '?' => escaped.push('.'),
            '.' | '+' | '^' | '$' | '{' | '}' | '(' | ')' | '|' | '[' | ']' => {
                escaped.push('\\');
                escaped.push(ch);
            }
            _ => escaped.push(ch),
        }
    }
    if let Some(stripped) = escaped.strip_suffix(" .*") {
        escaped = format!("{stripped}( .*)?");
    }
    let flags = if cfg!(windows) { "(?si)" } else { "(?s)" };
    Regex::new(&format!("{flags}^{escaped}$")).unwrap_or_else(|_| Regex::new("^$").unwrap())
}

pub fn matches(input: &str, pattern: &str) -> bool {
    let normalized = input.replace('\\', "/");
    let mut guard = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    let map = guard.get_or_insert_with(HashMap::new);
    if map.len() > 2048 {
        map.clear();
    }
    let re = map.entry(pattern.to_string()).or_insert_with(|| compile(pattern));
    re.is_match(&normalized)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basics() {
        assert!(matches("git status", "git *"));
        assert!(matches("git", "git *"));
        assert!(!matches("gitk", "git *"));
        assert!(matches("anything", "*"));
        assert!(matches("a.env", "*.env"));
        assert!(!matches("a.envx", "*.env"));
        assert!(matches("src/x.rs", "src/?.rs"));
        assert!(matches("multi\nline", "multi*"));
    }

    #[test]
    fn regex_metacharacters_are_literal() {
        // a pattern is a glob, never a regex: these must not widen the match
        assert!(matches("a.b", "a.b"));
        assert!(!matches("axb", "a.b"));
        assert!(matches("f(x)", "f(x)"));
        assert!(matches("[id]", "[id]"));
        assert!(!matches("i", "[id]"));
        assert!(matches("a+b", "a+b"));
        assert!(!matches("aab", "a+b"));
        assert!(matches("x$y", "x$y"));
        assert!(matches("a|b", "a|b"));
        assert!(!matches("a", "a|b"));
    }

    #[test]
    fn anchored_and_trailing_space_star() {
        // anchored at both ends
        assert!(!matches("xgit status", "git *"));
        assert!(!matches("git status; rm -rf /", "git status"));
        // `git *` matches `git` alone and `git <anything>`, not `git-foo`
        assert!(matches("git", "git *"));
        assert!(!matches("git-foo", "git *"));
        // `*` in the middle
        assert!(matches("npm run build", "npm run *"));
        assert!(matches("npm run", "npm run *"));
        assert!(!matches("npm runx", "npm run *"));
        // backslashes are normalised to slashes on both sides
        assert!(matches("src\\lib.rs", "src/*.rs"));
        // empty pattern only matches the empty input
        assert!(matches("", ""));
        assert!(!matches("a", ""));
    }
}
