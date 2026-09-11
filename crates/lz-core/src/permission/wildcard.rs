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
}
