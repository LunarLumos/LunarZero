//! Configuration loading: JSONC files merged in precedence order, `{env:}` /
//! `{file:}` substitution, and markdown-defined agents/commands.

pub mod markdown;
pub mod substitute;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use lz_schema::config::Config;
use serde_json::{Map, Value};

use crate::paths::{Paths, env_var};
use substitute::{Missing, substitute};

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("{path}: {message}")]
    Invalid { path: String, message: String },
    #[error("{path}: JSON error: {message}")]
    Json { path: String, message: String },
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// Result of loading configuration for one project directory.
#[derive(Debug, Clone)]
pub struct Loaded {
    pub config: Config,
    /// Raw merged JSON (keeps `agent`/`permission` key order and unknown keys).
    pub raw: Map<String, Value>,
    /// Every file that contributed, in merge order.
    pub sources: Vec<PathBuf>,
    /// Config directories (`~/.config/lunarzero`, `.opencode/`, …) that may hold
    /// `agent/`, `command/`, `skill/` markdown.
    pub directories: Vec<PathBuf>,
}

/// Deep-merge `source` into `target`. Objects recurse; everything else replaces.
pub fn merge_deep(target: &mut Value, source: Value) {
    match (target, source) {
        (Value::Object(t), Value::Object(s)) => {
            for (k, v) in s {
                match t.get_mut(&k) {
                    Some(existing) if existing.is_object() && v.is_object() => merge_deep(existing, v),
                    _ => {
                        t.insert(k, v);
                    }
                }
            }
        }
        (t, s) => *t = s,
    }
}

/// Merge with `instructions` arrays concatenated (deduped) rather than replaced.
fn merge_config(target: &mut Value, source: Value) {
    let prev = target.get("instructions").and_then(|v| v.as_array()).cloned();
    let next = source.get("instructions").and_then(|v| v.as_array()).cloned();
    merge_deep(target, source);
    if let (Some(a), Some(b)) = (prev, next) {
        let mut merged: Vec<Value> = Vec::new();
        for v in a.into_iter().chain(b) {
            if !merged.contains(&v) {
                merged.push(v);
            }
        }
        target["instructions"] = Value::Array(merged);
    }
}

pub fn parse_jsonc(text: &str, path: &Path) -> Result<Value, ConfigError> {
    let opts = jsonc_parser::ParseOptions {
        allow_comments: true,
        allow_trailing_commas: true,
        allow_loose_object_property_names: false,
    };
    let v = jsonc_parser::parse_to_serde_value(text, &opts).map_err(|e| ConfigError::Json {
        path: path.display().to_string(),
        message: e.to_string(),
    })?;
    Ok(v.unwrap_or(Value::Object(Map::new())))
}

/// Keys that were moved to `tui.json`; stripped from opencode.json on load.
const TUI_KEYS: &[&str] = &["theme", "keybinds", "tui"];

fn load_text(
    text: &str,
    path: &Path,
    dir: &Path,
    paths: &Paths,
    env: &HashMap<String, String>,
) -> Result<Value, ConfigError> {
    let text =
        substitute(text, dir, &paths.home, Missing::Error, env).map_err(|message| ConfigError::Invalid {
            path: path.display().to_string(),
            message,
        })?;
    let mut v = parse_jsonc(&text, path)?;
    if let Value::Object(m) = &mut v {
        for k in TUI_KEYS {
            m.remove(*k);
        }
    }
    Ok(v)
}

fn load_file(
    path: &Path,
    paths: &Paths,
    env: &HashMap<String, String>,
) -> Result<Option<Value>, ConfigError> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let dir = path.parent().unwrap_or(Path::new("."));
    load_text(&text, path, dir, paths, env).map(Some)
}

/// Walk from `start` up to `stop` (inclusive) and return directories, nearest first.
pub fn ancestors(start: &Path, stop: Option<&Path>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut cur = Some(start);
    while let Some(d) = cur {
        out.push(d.to_path_buf());
        if stop.is_some_and(|s| s == d) {
            break;
        }
        cur = d.parent();
    }
    out
}

/// Candidate config file names, lowest precedence first.
const FILE_NAMES: &[&str] = &[
    "opencode.json",
    "opencode.jsonc",
    "lunarzero.json",
    "lunarzero.jsonc",
];
const GLOBAL_FILE_NAMES: &[&str] = &[
    "config.json",
    "opencode.json",
    "opencode.jsonc",
    "lunarzero.json",
    "lunarzero.jsonc",
];
const DOT_DIRS: &[&str] = &[".opencode", ".lunarzero"];

pub struct LoadInput<'a> {
    pub paths: &'a Paths,
    pub directory: &'a Path,
    pub worktree: &'a Path,
}

pub fn load(input: LoadInput<'_>) -> Result<Loaded, ConfigError> {
    let LoadInput {
        paths,
        directory,
        worktree,
    } = input;
    let env = HashMap::new();
    let mut raw = Value::Object(Map::new());
    let mut sources = Vec::new();
    let mut merge = |raw: &mut Value, path: PathBuf, v: Value| {
        sources.push(path);
        merge_config(raw, v);
    };

    // 1. global config dirs (legacy first, lunarzero overrides)
    for dir in paths.global_config_dirs() {
        for name in GLOBAL_FILE_NAMES {
            let p = dir.join(name);
            if let Some(v) = load_file(&p, paths, &env)? {
                merge(&mut raw, p, v);
            }
        }
    }

    // 2. explicit config file
    if let Some(p) = env_var("CONFIG") {
        let p = PathBuf::from(p);
        if let Some(v) = load_file(&p, paths, &env)? {
            merge(&mut raw, p, v);
        }
    }

    let project_config_enabled = env_var("DISABLE_PROJECT_CONFIG").is_none();

    // 3. project files, root-first so the closest wins
    if project_config_enabled {
        let mut dirs = ancestors(directory, Some(worktree));
        dirs.reverse();
        for dir in &dirs {
            for name in FILE_NAMES {
                let p = dir.join(name);
                if let Some(v) = load_file(&p, paths, &env)? {
                    merge(&mut raw, p, v);
                }
            }
        }
    }

    // 4. config directories: global dirs, .opencode/.lunarzero up the tree, ~/.opencode, ~/.lunarzero
    let mut directories: Vec<PathBuf> = paths.global_config_dirs();
    if project_config_enabled {
        let mut dirs = ancestors(directory, Some(worktree));
        dirs.reverse();
        for dir in dirs {
            for dot in DOT_DIRS {
                let d = dir.join(dot);
                if d.is_dir() {
                    directories.push(d);
                }
            }
        }
    }
    for dot in DOT_DIRS {
        let d = paths.home.join(dot);
        if d.is_dir() && !directories.contains(&d) {
            directories.push(d);
        }
    }
    let mut seen = std::collections::HashSet::new();
    directories.retain(|d| seen.insert(d.clone()));

    for dir in &directories {
        let is_dot = dir
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| DOT_DIRS.contains(&n));
        if is_dot {
            for name in FILE_NAMES {
                let p = dir.join(name);
                if let Some(v) = load_file(&p, paths, &env)? {
                    merge(&mut raw, p, v);
                }
            }
        }
        let agents = load_markdown_entries(dir, &["agent", "agents"], "prompt");
        let modes = load_markdown_entries(dir, &["mode", "modes"], "prompt");
        let commands = load_markdown_entries(dir, &["command", "commands"], "template");
        let obj = raw.as_object_mut().expect("object");
        if !agents.is_empty() {
            let target = obj.entry("agent").or_insert_with(|| Value::Object(Map::new()));
            merge_deep(target, Value::Object(agents));
        }
        if !modes.is_empty() {
            let mut modes = modes;
            for (_, v) in modes.iter_mut() {
                if let Value::Object(m) = v {
                    m.insert("mode".into(), Value::String("primary".into()));
                }
            }
            let target = obj.entry("agent").or_insert_with(|| Value::Object(Map::new()));
            merge_deep(target, Value::Object(modes));
        }
        if !commands.is_empty() {
            let target = obj.entry("command").or_insert_with(|| Value::Object(Map::new()));
            merge_deep(target, Value::Object(commands));
        }
    }

    // 5. inline content
    if let Some(content) = env_var("CONFIG_CONTENT") {
        let v = load_text(&content, Path::new("LZ_CONFIG_CONTENT"), directory, paths, &env)?;
        merge(&mut raw, PathBuf::from("LZ_CONFIG_CONTENT"), v);
    }

    post_process(&mut raw);

    let raw_map = match raw {
        Value::Object(m) => m,
        _ => Map::new(),
    };
    let config: Config =
        serde_json::from_value(Value::Object(raw_map.clone())).map_err(|e| ConfigError::Json {
            path: "<merged config>".into(),
            message: e.to_string(),
        })?;
    Ok(Loaded {
        config,
        raw: raw_map,
        sources,
        directories,
    })
}

/// `mode` → `agent` (primary), `LZ_PERMISSION` env, `tools` → `permission`, username default.
fn post_process(raw: &mut Value) {
    let Some(obj) = raw.as_object_mut() else { return };

    if let Some(Value::Object(modes)) = obj.remove("mode") {
        let target = obj.entry("agent").or_insert_with(|| Value::Object(Map::new()));
        for (name, mut cfg) in modes {
            if let Value::Object(m) = &mut cfg {
                m.insert("mode".into(), Value::String("primary".into()));
            }
            merge_deep(target, Value::Object(Map::from_iter([(name, cfg)])));
        }
    }

    if let Some(p) = env_var("PERMISSION")
        && let Ok(v) = serde_json::from_str::<Value>(&p)
    {
        let target = obj
            .entry("permission")
            .or_insert_with(|| Value::Object(Map::new()));
        merge_deep(target, v);
    }

    if let Some(Value::Object(tools)) = obj.get("tools").cloned() {
        let mut perms = Map::new();
        for (tool, enabled) in tools {
            let action = if enabled.as_bool().unwrap_or(false) {
                "allow"
            } else {
                "deny"
            };
            let key = if matches!(tool.as_str(), "write" | "edit" | "patch") {
                "edit".to_string()
            } else {
                tool
            };
            perms.insert(key, Value::String(action.into()));
        }
        let existing = obj.remove("permission").unwrap_or(Value::Object(Map::new()));
        let mut merged = Value::Object(perms);
        merge_deep(&mut merged, existing);
        obj.insert("permission".into(), merged);
    }

    if !obj.contains_key("username") {
        let name = std::env::var("USER")
            .or_else(|_| std::env::var("USERNAME"))
            .unwrap_or_else(|_| "user".into());
        obj.insert("username".into(), Value::String(name));
    }

    if env_var("DISABLE_AUTOCOMPACT").is_some() {
        let c = obj
            .entry("compaction")
            .or_insert_with(|| Value::Object(Map::new()));
        merge_deep(c, serde_json::json!({ "auto": false }));
    }
    if env_var("DISABLE_PRUNE").is_some() {
        let c = obj
            .entry("compaction")
            .or_insert_with(|| Value::Object(Map::new()));
        merge_deep(c, serde_json::json!({ "prune": false }));
    }
}

/// Load `<dir>/{prefixes}/**/*.md` as `{name: {frontmatter..., <body_key>: body}}`.
fn load_markdown_entries(dir: &Path, prefixes: &[&str], body_key: &str) -> Map<String, Value> {
    let mut out = Map::new();
    for prefix in prefixes {
        let root = dir.join(prefix);
        if !root.is_dir() {
            continue;
        }
        let walker = ignore::WalkBuilder::new(&root)
            .hidden(false)
            .git_ignore(false)
            .git_global(false)
            .git_exclude(false)
            .follow_links(true)
            .build();
        for entry in walker.flatten() {
            let p = entry.path();
            if !p.is_file() || p.extension().and_then(|e| e.to_str()) != Some("md") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(p) else {
                continue;
            };
            let Ok(md) = markdown::parse(&text) else { continue };
            let rel = p.strip_prefix(dir).unwrap_or(p).to_string_lossy().to_string();
            let strip: Vec<String> = prefixes.iter().map(|x| format!("{x}/")).collect();
            let strip_refs: Vec<&str> = strip.iter().map(String::as_str).collect();
            let name = markdown::entry_name(&rel, &strip_refs);
            let mut obj = Map::new();
            obj.insert("name".into(), Value::String(name.clone()));
            for (k, v) in md.data {
                obj.insert(k, v);
            }
            obj.insert(body_key.into(), Value::String(md.content.trim().to_string()));
            out.insert(name, Value::Object(obj));
        }
    }
    out
}

/// Load `tui.json[c]` from global dirs then project `.opencode/.lunarzero` dirs.
pub fn load_tui(paths: &Paths, directories: &[PathBuf]) -> lz_schema::config::TuiConfig {
    let mut raw = Value::Object(Map::new());
    let env = HashMap::new();
    let mut dirs: Vec<PathBuf> = paths.global_config_dirs();
    dirs.extend(directories.iter().cloned());
    if let Some(p) = env_var("TUI_CONFIG") {
        dirs.push(
            PathBuf::from(p)
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_default(),
        );
    }
    for dir in dirs {
        for name in ["tui.json", "tui.jsonc"] {
            let p = dir.join(name);
            let Ok(text) = std::fs::read_to_string(&p) else {
                continue;
            };
            let Ok(text) = substitute(&text, &dir, &paths.home, Missing::Empty, &env) else {
                continue;
            };
            if let Ok(v) = parse_jsonc(&text, &p) {
                merge_deep(&mut raw, v);
            }
        }
    }
    serde_json::from_value(raw).unwrap_or_default()
}

pub fn json_schema() -> Value {
    serde_json::to_value(schemars::schema_for!(Config)).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths_in(tmp: &Path) -> Paths {
        Paths {
            home: tmp.join("home"),
            config: tmp.join("home/.config/lunarzero"),
            data: tmp.join("home/.local/share/lunarzero"),
            cache: tmp.join("home/.cache/lunarzero"),
            state: tmp.join("home/.local/state/lunarzero"),
            legacy_config: tmp.join("home/.config/opencode"),
            legacy_data: tmp.join("home/.local/share/opencode"),
        }
    }

    #[test]
    fn precedence_and_markdown_agents() {
        let tmp = tempfile::tempdir().unwrap();
        let p = paths_in(tmp.path());
        std::fs::create_dir_all(&p.legacy_config).unwrap();
        std::fs::write(
            p.legacy_config.join("opencode.json"),
            r#"{ "model": "openai/gpt-4o", "instructions": ["a.md"], "permission": { "bash": "ask" } }"#,
        )
        .unwrap();
        let proj = tmp.path().join("proj");
        std::fs::create_dir_all(proj.join(".opencode/agent")).unwrap();
        std::fs::write(
            proj.join("opencode.jsonc"),
            "{ // comment\n \"model\": \"groq/llama\", \"instructions\": [\"b.md\"], \"tools\": {\"write\": false} }",
        )
        .unwrap();
        std::fs::write(
            proj.join(".opencode/agent/review.md"),
            "---\ndescription: reviews code\nmode: subagent\n---\nYou review.",
        )
        .unwrap();
        let loaded = load(LoadInput {
            paths: &p,
            directory: &proj,
            worktree: &proj,
        })
        .unwrap();
        assert_eq!(loaded.config.model.as_deref(), Some("groq/llama"));
        assert_eq!(
            loaded.config.instructions.as_ref().unwrap(),
            &vec!["a.md".to_string(), "b.md".to_string()]
        );
        let agent = &loaded.raw["agent"]["review"];
        assert_eq!(agent["prompt"], "You review.");
        assert_eq!(agent["mode"], "subagent");
        // tools → permission (edit: deny), user permission preserved
        let perm = loaded.raw["permission"].as_object().unwrap();
        assert_eq!(perm["edit"], "deny");
        assert_eq!(perm["bash"], "ask");
    }

    #[test]
    fn merge_deep_recurses_objects() {
        let mut a = serde_json::json!({"x": {"a": 1, "b": 2}, "y": [1]});
        merge_deep(&mut a, serde_json::json!({"x": {"b": 3}, "y": [2]}));
        assert_eq!(a, serde_json::json!({"x": {"a": 1, "b": 3}, "y": [2]}));
    }
}
