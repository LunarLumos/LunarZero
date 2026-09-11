//! Per-model quirks:
//! sampling defaults, output-token cap, reasoning variants, tool-schema
//! sanitizing for strict OpenAI-style validators.

use std::collections::BTreeMap;

use serde_json::{Map, Value, json};

use super::Model;
use super::catalog::ReasoningOption;

pub const OUTPUT_TOKEN_MAX: u64 = 32_000;

pub fn temperature(model: &Model) -> Option<f64> {
    let id = model.api_id.to_lowercase();
    if id.contains("claude") {
        return None;
    }
    if id.contains("glm-4.6") || id.contains("glm-4.7") || id.contains("minimax-m2") {
        return Some(1.0);
    }
    if id.contains("kimi-k2") {
        if ["thinking", "k2.", "k2p", "k2-5"].iter().any(|s| id.contains(s)) {
            return Some(1.0);
        }
        return Some(0.6);
    }
    None
}

pub fn top_p(model: &Model) -> Option<f64> {
    let id = model.api_id.to_lowercase();
    if ["minimax-m2", "kimi-k2.5", "kimi-k2p5", "kimi-k2-5"]
        .iter()
        .any(|s| id.contains(s))
    {
        return Some(0.95);
    }
    None
}

pub fn max_output_tokens(model: &Model) -> u64 {
    let limit = model.limit.output as u64;
    let v = limit.min(OUTPUT_TOKEN_MAX);
    if v == 0 { OUTPUT_TOKEN_MAX } else { v }
}

/// Provider-option patch that enables a given reasoning effort for this model.
fn reasoning_effort(model: &Model, effort: &str) -> Option<Map<String, Value>> {
    let obj = match model.npm.as_str() {
        "@openrouter/ai-sdk-provider" => json!({ "reasoning": { "effort": effort } }),
        _ => json!({ "reasoningEffort": effort }),
    };
    obj.as_object().cloned()
}

/// Derive the variant table (`high`, `medium`, …) from catalog `reasoning_options`.
pub fn variants(model: &Model, options: Option<&[ReasoningOption]>) -> BTreeMap<String, Map<String, Value>> {
    let mut out = BTreeMap::new();
    let Some(options) = options else { return out };
    if let Some(effort) = options.iter().find(|o| o.kind == "effort") {
        for v in &effort.values {
            let id = match v {
                Value::Null => "none".to_string(),
                Value::String(s) => s.clone(),
                _ => continue,
            };
            if let Some(settings) = reasoning_effort(model, &id) {
                out.insert(id, settings);
            }
        }
        return out;
    }
    // toggle-style reasoning (openai-compatible servers rarely support this; expose on/off)
    if options.iter().any(|o| o.kind == "toggle") {
        out.insert(
            "none".into(),
            json!({ "reasoning": { "enabled": false } })
                .as_object()
                .cloned()
                .unwrap(),
        );
        out.insert(
            "high".into(),
            json!({ "reasoning": { "enabled": true } })
                .as_object()
                .cloned()
                .unwrap(),
        );
    }
    out
}

/// Turn a variant's option patch into `provider_options` for the request.
pub fn provider_options(model: &Model, variant: Option<&str>) -> Map<String, Value> {
    let mut openai = Map::new();
    for (k, v) in &model.options {
        openai.insert(k.clone(), v.clone());
    }
    if let Some(v) = variant.and_then(|v| model.variants.get(v)) {
        for (k, val) in v {
            openai.insert(k.clone(), val.clone());
        }
    }
    let mut out = Map::new();
    if !openai.is_empty() {
        // OpenRouter uses a top-level `reasoning` body field; pass it raw.
        if let Some(reasoning) = openai.remove("reasoning") {
            out.insert("raw".into(), json!({ "reasoning": reasoning }));
        }
        out.insert("openai".into(), Value::Object(openai));
    }
    out
}

/// Make a JSON schema acceptable to strict OpenAI-style validators
///.
pub fn sanitize_schema(value: &Value) -> Value {
    match value {
        Value::Bool(true) => json!({}),
        Value::Bool(false) => json!({ "not": {} }),
        Value::Array(items) => Value::Array(items.iter().map(sanitize_schema).collect()),
        Value::Object(obj) => {
            let mut out = Map::new();
            for (k, v) in obj {
                match k.as_str() {
                    // unsupported keywords for OpenAI tool schemas
                    "$schema" | "$id" | "title" | "examples" | "default" | "format" | "$comment"
                    | "minLength" | "maxLength" | "pattern" | "minimum" | "maximum" | "exclusiveMinimum"
                    | "exclusiveMaximum" | "multipleOf" | "minItems" | "maxItems" | "uniqueItems"
                    | "minProperties" | "maxProperties" => continue,
                    // maps whose keys are *names*, not keywords (a property may be called `pattern`)
                    "properties" | "$defs" | "definitions" | "patternProperties" => {
                        if let Value::Object(props) = v {
                            let cleaned: Map<String, Value> = props
                                .iter()
                                .map(|(name, schema)| (name.clone(), sanitize_schema(schema)))
                                .collect();
                            out.insert(k.clone(), Value::Object(cleaned));
                        } else {
                            out.insert(k.clone(), v.clone());
                        }
                        continue;
                    }
                    _ => {}
                }
                out.insert(k.clone(), sanitize_schema(v));
            }
            if out.get("type").and_then(Value::as_str) == Some("object")
                && !out.contains_key("additionalProperties")
            {
                out.insert("additionalProperties".into(), Value::Bool(false));
            }
            // `type: ["string", "null"]` → anyOf
            if let Some(Value::Array(types)) = out.get("type").cloned()
                && types.len() > 1
            {
                let any: Vec<Value> = types.into_iter().map(|t| json!({ "type": t })).collect();
                out.remove("type");
                out.insert("anyOf".into(), Value::Array(any));
            }
            Value::Object(out)
        }
        other => other.clone(),
    }
}

/// System-prompt family used to pick the base prompt file.
pub fn prompt_family(model: &Model) -> &'static str {
    let id = model.api_id.to_lowercase();
    if id.contains("gpt-4")
        || id.starts_with("o1")
        || id.starts_with("o3")
        || id.contains("/o1")
        || id.contains("/o3")
    {
        return "beast";
    }
    if id.contains("gpt-6") {
        return "gpt-astra";
    }
    if id.contains("codex") {
        return "codex";
    }
    if id.contains("gpt") {
        return "gpt";
    }
    if id.contains("gemini") {
        return "gemini";
    }
    if id.contains("claude") {
        return "anthropic";
    }
    if id.contains("trinity") {
        return "trinity";
    }
    if id.contains("kimi") || id.contains("moonshot") {
        return "kimi";
    }
    "default"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_sanitizing() {
        let s = json!({
            "type": "object",
            "properties": { "a": { "type": "string", "format": "uri", "minLength": 1 }, "b": { "type": ["number", "null"] } },
            "required": ["a"]
        });
        let out = sanitize_schema(&s);
        assert_eq!(out["additionalProperties"], false);
        assert!(out["properties"]["a"].get("format").is_none());
        assert!(out["properties"]["b"]["anyOf"].is_array());
    }
}

#[cfg(test)]
mod sanitize_tests {
    use super::*;

    #[test]
    fn property_named_pattern_survives_sanitizing() {
        let schema = json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "pattern": "^a", "description": "glob" },
                "title": { "type": "string" }
            },
            "required": ["pattern"]
        });
        let out = sanitize_schema(&schema);
        assert!(out["properties"]["pattern"].is_object());
        assert!(out["properties"]["title"].is_object());
        assert!(out["properties"]["pattern"].get("pattern").is_none());
        assert_eq!(out["additionalProperties"], json!(false));
    }
}
