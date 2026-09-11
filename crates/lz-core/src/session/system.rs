//! System prompt assembly: base prompt by model family, environment block,
//! instruction files (AGENTS.md / CLAUDE.md / config `instructions`), plus
//! hooks for MCP instructions and skills.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use lz_schema::config::Config;

use crate::paths::{Paths, env_var, expand_home};
use crate::provider::Model;
use crate::provider::transform::prompt_family;

macro_rules! prompt {
    ($name:literal) => {
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../assets/prompts/system/",
            $name,
            ".md"
        ))
    };
}

/// LunarZero's core system prompt, shared by every model.
pub const PROMPT_CORE: &str = prompt!("core");
pub const PROMPT_FAMILY_GPT: &str = prompt!("family-gpt");
pub const PROMPT_FAMILY_GEMINI: &str = prompt!("family-gemini");
pub const PROMPT_FAMILY_OPEN: &str = prompt!("family-open");
pub const PROMPT_PLAN: &str = prompt!("plan-mode");
pub const PROMPT_BUILD_SWITCH: &str = prompt!("build-mode");

pub const MAX_STEPS_PROMPT: &str = "Step limit reached for this turn: tools are now disabled. Reply in text only — say what you completed, what you verified, and what is still left. Do not attempt any tool call; wait for the user's next message.";

pub const STRUCTURED_OUTPUT_PROMPT: &str = "The caller wants a JSON result. When the task is finished, call the StructuredOutput tool exactly once with a value that matches the requested schema instead of answering in prose.";

/// Base prompt for a model (used when the agent has no custom prompt): the
/// shared core plus a few lines of family-specific steering.
pub fn base_prompt(model: &Model) -> String {
    let addendum = match prompt_family(model) {
        "beast" | "gpt-astra" | "codex" | "gpt" => PROMPT_FAMILY_GPT,
        "gemini" => PROMPT_FAMILY_GEMINI,
        "anthropic" => "",
        _ => PROMPT_FAMILY_OPEN,
    };
    if addendum.is_empty() {
        PROMPT_CORE.trim_end().to_string()
    } else {
        format!("{}\n\n{}", PROMPT_CORE.trim_end(), addendum.trim_end())
    }
}

pub struct EnvInput<'a> {
    pub model: &'a Model,
    pub directory: &'a Path,
    pub worktree: &'a Path,
    pub is_git: bool,
}

pub fn environment(input: EnvInput<'_>) -> String {
    let date = jiff::Zoned::now().strftime("%a %b %d %Y").to_string();
    format!(
        "You are powered by the model named {}. The exact model ID is {}/{}\nHere is some useful information about the environment you are running in:\n<env>\n  Working directory: {}\n  Workspace root folder: {}\n  Is directory a git repo: {}\n  Platform: {}\n  Today's date: {}\n</env>",
        input.model.api_id,
        input.model.provider_id,
        input.model.api_id,
        input.directory.display(),
        input.worktree.display(),
        if input.is_git { "yes" } else { "no" },
        std::env::consts::OS,
        date
    )
}

const INSTRUCTION_FILES: &[&str] = &["AGENTS.md", "CLAUDE.md", "CONTEXT.md"];

fn claude_disabled() -> bool {
    env_var("DISABLE_CLAUDE_CODE").is_some() || env_var("DISABLE_CLAUDE_CODE_PROMPT").is_some()
}

/// Every instruction file path that belongs in the system prompt.
pub fn instruction_paths(paths: &Paths, config: &Config, directory: &Path, worktree: &Path) -> Vec<PathBuf> {
    let mut out: BTreeSet<PathBuf> = BTreeSet::new();
    let mut ordered: Vec<PathBuf> = Vec::new();
    let mut push = |p: PathBuf| {
        if out.insert(p.clone()) {
            ordered.push(p);
        }
    };

    // global: first existing of lunarzero/AGENTS.md, opencode/AGENTS.md, ~/.claude/CLAUDE.md
    let mut globals = vec![
        paths.config.join("AGENTS.md"),
        paths.legacy_config.join("AGENTS.md"),
    ];
    if !claude_disabled() {
        globals.push(paths.home.join(".claude").join("CLAUDE.md"));
    }
    if let Some(g) = globals.into_iter().find(|p| p.is_file()) {
        push(g);
    }

    // project: first file name that matches anywhere from cwd up to worktree wins
    if env_var("DISABLE_PROJECT_CONFIG").is_none() {
        let names: Vec<&str> = INSTRUCTION_FILES
            .iter()
            .copied()
            .filter(|n| *n != "CLAUDE.md" || !claude_disabled())
            .collect();
        let dirs = crate::config::ancestors(directory, Some(worktree));
        for name in names {
            let matches: Vec<PathBuf> = dirs
                .iter()
                .map(|d| d.join(name))
                .filter(|p| p.is_file())
                .collect();
            if !matches.is_empty() {
                for m in matches {
                    push(m);
                }
                break;
            }
        }
    }

    // config.instructions: globs (relative → searched upward), absolute, ~
    for raw in config.instructions.iter().flatten() {
        if raw.starts_with("http://") || raw.starts_with("https://") {
            continue;
        }
        let expanded = expand_home(raw, &paths.home);
        let candidates: Vec<PathBuf> = if expanded.is_absolute() {
            glob_in(
                expanded.parent().unwrap_or(Path::new("/")),
                expanded.file_name().and_then(|f| f.to_str()).unwrap_or(""),
            )
        } else {
            let mut found = Vec::new();
            for d in crate::config::ancestors(directory, Some(worktree)) {
                found.extend(glob_rel(&d, raw));
            }
            found
        };
        for c in candidates {
            push(c);
        }
    }
    ordered
}

fn glob_in(dir: &Path, pattern: &str) -> Vec<PathBuf> {
    let Ok(g) = globset::Glob::new(pattern) else {
        return Vec::new();
    };
    let m = g.compile_matcher();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.file_name().is_some_and(|n| m.is_match(n)))
        .collect();
    out.sort();
    out
}

fn glob_rel(dir: &Path, pattern: &str) -> Vec<PathBuf> {
    let Ok(g) = globset::GlobBuilder::new(pattern).literal_separator(true).build() else {
        return Vec::new();
    };
    let m = g.compile_matcher();
    let mut out = Vec::new();
    let walker = ignore::WalkBuilder::new(dir)
        .hidden(false)
        .max_depth(Some(6))
        .build();
    for e in walker.flatten() {
        let p = e.path();
        if !p.is_file() {
            continue;
        }
        let rel = p.strip_prefix(dir).unwrap_or(p);
        if m.is_match(rel) {
            out.push(p.to_path_buf());
        }
    }
    out.sort();
    out
}

/// Render instruction blocks (`Instructions from: <path>\n<content>`).
pub async fn instructions(paths: &Paths, config: &Config, directory: &Path, worktree: &Path) -> Vec<String> {
    let mut out = Vec::new();
    for p in instruction_paths(paths, config, directory, worktree) {
        if let Ok(content) = std::fs::read_to_string(&p)
            && !content.trim().is_empty()
        {
            out.push(format!("Instructions from: {}\n{}", p.display(), content));
        }
    }
    let urls: Vec<&String> = config
        .instructions
        .iter()
        .flatten()
        .filter(|s| s.starts_with("http://") || s.starts_with("https://"))
        .collect();
    if !urls.is_empty() {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .ok();
        for url in urls {
            let Some(client) = &client else { break };
            if let Ok(resp) = client.get(url).send().await
                && let Ok(text) = resp.text().await
                && !text.trim().is_empty()
            {
                out.push(format!("Instructions from: {url}\n{text}"));
            }
        }
    }
    out
}

/// Nearest instruction file in `dir` (used for lazy nested AGENTS.md).
pub fn find_in(dir: &Path) -> Option<PathBuf> {
    INSTRUCTION_FILES
        .iter()
        .filter(|n| **n != "CLAUDE.md" || !claude_disabled())
        .map(|n| dir.join(n))
        .find(|p| p.is_file())
}
