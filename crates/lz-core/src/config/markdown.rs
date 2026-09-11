//! Markdown files with YAML frontmatter (agents, commands, skills).

/// `@path` references in command templates.
pub const FILE_REGEX: &str = r"(?:^|[^\w`])@(\.?[^\s`,.]*(?:\.[^\s`,.]+)*)";

use serde_json::{Map, Value};

#[derive(Debug, Clone, Default)]
pub struct Markdown {
    pub data: Map<String, Value>,
    pub content: String,
}

fn split(content: &str) -> Option<(&str, &str)> {
    let rest = content.strip_prefix("---")?;
    let rest = rest.strip_prefix("\r\n").or_else(|| rest.strip_prefix('\n'))?;
    // find closing `---` at a line start
    let mut idx = 0;
    for line in rest.split_inclusive('\n') {
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed == "---" {
            let body = &rest[idx + line.len()..];
            return Some((&rest[..idx], body));
        }
        idx += line.len();
    }
    None
}

/// Other agents accept unquoted colons in frontmatter values; rewrite those as
/// block scalars so the YAML still parses.
pub fn sanitize(frontmatter: &str) -> String {
    frontmatter
        .lines()
        .flat_map(|line| {
            let t = line.trim();
            if t.starts_with('#') || t.is_empty() || line.starts_with(char::is_whitespace) {
                return vec![line.to_string()];
            }
            let Some((key, value)) = line.split_once(':') else {
                return vec![line.to_string()];
            };
            let key = key.trim();
            if key.is_empty()
                || !key
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                || !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            {
                return vec![line.to_string()];
            }
            let value = value.trim();
            if value.is_empty()
                || value == ">"
                || value == "|"
                || value.starts_with('"')
                || value.starts_with('\'')
                || !value.contains(':')
            {
                return vec![line.to_string()];
            }
            vec![format!("{key}: |-"), format!("  {value}")]
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn yaml_to_json(v: serde_yaml_ng::Value) -> Value {
    match v {
        serde_yaml_ng::Value::Null => Value::Null,
        serde_yaml_ng::Value::Bool(b) => Value::Bool(b),
        serde_yaml_ng::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::from(i)
            } else if let Some(u) = n.as_u64() {
                Value::from(u)
            } else {
                n.as_f64().map(Value::from).unwrap_or(Value::Null)
            }
        }
        serde_yaml_ng::Value::String(s) => Value::String(s),
        serde_yaml_ng::Value::Sequence(seq) => Value::Array(seq.into_iter().map(yaml_to_json).collect()),
        serde_yaml_ng::Value::Mapping(m) => Value::Object(
            m.into_iter()
                .map(|(k, v)| {
                    let key = match k {
                        serde_yaml_ng::Value::String(s) => s,
                        other => serde_yaml_ng::to_string(&other)
                            .unwrap_or_default()
                            .trim()
                            .to_string(),
                    };
                    (key, yaml_to_json(v))
                })
                .collect(),
        ),
        serde_yaml_ng::Value::Tagged(t) => yaml_to_json(t.value),
    }
}

fn parse_yaml(text: &str) -> Result<Map<String, Value>, serde_yaml_ng::Error> {
    if text.trim().is_empty() {
        return Ok(Map::new());
    }
    let v: serde_yaml_ng::Value = serde_yaml_ng::from_str(text)?;
    Ok(match yaml_to_json(v) {
        Value::Object(m) => m,
        _ => Map::new(),
    })
}

pub fn parse(content: &str) -> Result<Markdown, String> {
    let Some((fm, body)) = split(content) else {
        return Ok(Markdown {
            data: Map::new(),
            content: content.to_string(),
        });
    };
    let data = match parse_yaml(fm) {
        Ok(d) => d,
        Err(_) => parse_yaml(&sanitize(fm)).map_err(|e| e.to_string())?,
    };
    Ok(Markdown {
        data,
        content: body.to_string(),
    })
}

/// `agent/foo/bar.md` relative to a config dir → `foo/bar`.
pub fn entry_name(relative: &str, prefixes: &[&str]) -> String {
    let normalized = relative.replace('\\', "/");
    let candidate = prefixes
        .iter()
        .find_map(|p| normalized.strip_prefix(p))
        .map(str::to_string)
        .unwrap_or_else(|| normalized.rsplit('/').next().unwrap_or(&normalized).to_string());
    match candidate.rfind('.') {
        Some(i) if !candidate[i + 1..].contains('/') => candidate[..i].to_string(),
        _ => candidate,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_frontmatter_and_body() {
        let md = parse("---\ndescription: hi\nmode: subagent\n---\nBody text\n").unwrap();
        assert_eq!(md.data["description"], "hi");
        assert_eq!(md.content.trim(), "Body text");
    }

    #[test]
    fn no_frontmatter() {
        let md = parse("just text").unwrap();
        assert!(md.data.is_empty());
        assert_eq!(md.content, "just text");
    }

    #[test]
    fn sanitizes_unquoted_colons() {
        let md = parse("---\ndescription: Use this: always\n---\nx").unwrap();
        assert_eq!(md.data["description"], "Use this: always");
    }

    #[test]
    fn entry_names() {
        assert_eq!(entry_name("agent/review.md", &["agent/", "agents/"]), "review");
        assert_eq!(
            entry_name("commands/git/commit.md", &["command/", "commands/"]),
            "git/commit"
        );
    }
}
