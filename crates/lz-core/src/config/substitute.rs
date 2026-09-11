//! `{env:VAR}` and `{file:path}` substitution in config text.

use std::collections::HashMap;
use std::path::Path;

use crate::paths::expand_home;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Missing {
    Error,
    Empty,
}

pub fn substitute(
    text: &str,
    config_dir: &Path,
    home: &Path,
    missing: Missing,
    env: &HashMap<String, String>,
) -> Result<String, String> {
    let text = replace_tokens(text, "{env:", |name, _| {
        Ok(env
            .get(name)
            .cloned()
            .or_else(|| std::env::var(name).ok())
            .unwrap_or_default())
    })?;
    replace_tokens(&text, "{file:", |raw, on_comment_line| {
        if on_comment_line {
            return Err(String::new()); // signal "leave token as is"
        }
        let p = expand_home(raw, home);
        let resolved = if p.is_absolute() { p } else { config_dir.join(p) };
        match std::fs::read_to_string(&resolved) {
            Ok(content) => Ok(serde_json::to_string(content.trim())
                .map(|s| s[1..s.len() - 1].to_string())
                .unwrap_or_default()),
            Err(_) if missing == Missing::Empty => Ok(String::new()),
            Err(e) => Err(format!(
                "bad file reference: \"{{file:{raw}}}\" {} ({e})",
                resolved.display()
            )),
        }
    })
}

fn replace_tokens(
    text: &str,
    open: &str,
    mut f: impl FnMut(&str, bool) -> Result<String, String>,
) -> Result<String, String> {
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0;
    while let Some(rel) = text[cursor..].find(open) {
        let start = cursor + rel;
        let Some(end_rel) = text[start..].find('}') else {
            break;
        };
        let end = start + end_rel;
        let inner = &text[start + open.len()..end];
        let line_start = text[..start].rfind('\n').map(|i| i + 1).unwrap_or(0);
        let on_comment = text[line_start..start].trim_start().starts_with("//");
        out.push_str(&text[cursor..start]);
        match f(inner, on_comment) {
            Ok(v) => out.push_str(&v),
            Err(e) if e.is_empty() => out.push_str(&text[start..=end]),
            Err(e) => return Err(e),
        }
        cursor = end + 1;
    }
    out.push_str(&text[cursor..]);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_substitution() {
        let mut env = HashMap::new();
        env.insert("FOO".to_string(), "bar".to_string());
        let out = substitute(
            "{\"k\":\"{env:FOO}-{env:NOPE}\"}",
            Path::new("/"),
            Path::new("/"),
            Missing::Empty,
            &env,
        )
        .unwrap();
        assert_eq!(out, "{\"k\":\"bar-\"}");
    }

    #[test]
    fn file_substitution() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("secret.txt"), "s3cr3t\n").unwrap();
        let out = substitute(
            "{\"k\":\"{file:secret.txt}\"}",
            dir.path(),
            Path::new("/"),
            Missing::Error,
            &HashMap::new(),
        )
        .unwrap();
        assert_eq!(out, "{\"k\":\"s3cr3t\"}");
        assert!(
            substitute(
                "{file:missing.txt}",
                dir.path(),
                Path::new("/"),
                Missing::Error,
                &HashMap::new()
            )
            .is_err()
        );
        let kept = substitute(
            "// {file:missing.txt}\n{}",
            dir.path(),
            Path::new("/"),
            Missing::Error,
            &HashMap::new(),
        )
        .unwrap();
        assert_eq!(kept, "// {file:missing.txt}\n{}");
    }
}
