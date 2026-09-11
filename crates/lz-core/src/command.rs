//! Slash commands: built-in `init`/`review`, config/markdown commands, and
//! skills exposed as commands. Templates support `$ARGUMENTS`, `$1..$N`,
//! `` !`shell` `` and `@path` file references.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use lz_schema::api::CommandInfo;
use lz_schema::config::AgentMode;
use lz_schema::session::*;

use crate::config::markdown;
use crate::engine::Engine;

pub const PROMPT_INITIALIZE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../assets/prompts/commands/init.md"
));
pub const PROMPT_REVIEW: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../assets/prompts/commands/review.md"
));

#[derive(Debug, Clone)]
pub struct Command {
    pub name: String,
    pub description: Option<String>,
    pub agent: Option<String>,
    pub model: Option<String>,
    pub template: String,
    pub subtask: Option<bool>,
    pub source: &'static str,
}

impl Command {
    pub fn to_info(&self) -> CommandInfo {
        CommandInfo {
            name: self.name.clone(),
            description: self.description.clone(),
            agent: self.agent.clone(),
            model: self.model.clone(),
            subtask: self.subtask.unwrap_or(false),
            template: self.template.clone(),
            source: self.source.into(),
        }
    }
}

pub fn build(
    raw_config: &serde_json::Map<String, serde_json::Value>,
    worktree: &Path,
    skills: &BTreeMap<String, crate::skill::Skill>,
) -> BTreeMap<String, Command> {
    let mut out = BTreeMap::new();
    let wt = worktree.display().to_string();
    out.insert(
        "init".into(),
        Command {
            name: "init".into(),
            description: Some("guided AGENTS.md setup".into()),
            agent: None,
            model: None,
            template: PROMPT_INITIALIZE.replace("${path}", &wt),
            subtask: None,
            source: "builtin",
        },
    );
    out.insert(
        "review".into(),
        Command {
            name: "review".into(),
            description: Some("review changes [commit|branch|pr], defaults to uncommitted".into()),
            agent: None,
            model: None,
            template: PROMPT_REVIEW.replace("${path}", &wt),
            subtask: Some(true),
            source: "builtin",
        },
    );
    if let Some(serde_json::Value::Object(cmds)) = raw_config.get("command") {
        for (name, v) in cmds {
            let Some(template) = v.get("template").and_then(|t| t.as_str()) else {
                continue;
            };
            out.insert(
                name.clone(),
                Command {
                    name: name.clone(),
                    description: v.get("description").and_then(|d| d.as_str()).map(str::to_string),
                    agent: v.get("agent").and_then(|d| d.as_str()).map(str::to_string),
                    model: v.get("model").and_then(|d| d.as_str()).map(str::to_string),
                    template: template.to_string(),
                    subtask: v.get("subtask").and_then(|d| d.as_bool()),
                    source: "config",
                },
            );
        }
    }
    for (name, skill) in skills {
        if out.contains_key(name) {
            continue;
        }
        let dir = skill
            .location
            .parent()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        out.insert(
            name.clone(),
            Command {
                name: name.clone(),
                description: skill.description.clone(),
                agent: None,
                model: None,
                template: format!(
                    "{}\n\nBase directory for this skill: {dir}\nRelative paths in this skill (e.g., scripts/, references/) are relative to this base directory.",
                    skill.content
                ),
                subtask: None,
                source: "skill",
            },
        );
    }
    out
}

/// Split an argument string: `[Image N]` tokens,
/// quoted strings, and whitespace-separated words.
pub fn split_args(s: &str) -> Vec<String> {
    let re = regex::Regex::new(r#"(?i)(?:\[Image\s+\d+\]|"[^"]*"|'[^']*'|[^\s"']+)"#).unwrap();
    re.find_iter(s)
        .map(|m| m.as_str().trim_matches(|c| c == '"' || c == '\'').to_string())
        .collect()
}

/// Substitute `$1..$N` / `$ARGUMENTS` into a template.
pub fn substitute(template: &str, arguments: &str) -> String {
    let args = split_args(arguments);
    let placeholder = regex::Regex::new(r"\$(\d+)").unwrap();
    let last = placeholder
        .captures_iter(template)
        .filter_map(|c| c[1].parse::<usize>().ok())
        .max()
        .unwrap_or(0);
    let with_args = placeholder.replace_all(template, |c: &regex::Captures| {
        let position: usize = c[1].parse().unwrap_or(0);
        let idx = position.saturating_sub(1);
        if idx >= args.len() {
            return String::new();
        }
        if position == last {
            args[idx..].join(" ")
        } else {
            args[idx].clone()
        }
    });
    let uses_arguments = template.contains("$ARGUMENTS");
    let mut out = with_args.replace("$ARGUMENTS", arguments);
    if last == 0 && !uses_arguments && !arguments.trim().is_empty() {
        out.push_str("\n\n");
        out.push_str(arguments);
    }
    out
}

/// Run `` !`cmd` `` blocks through the shell and splice their output in.
pub async fn expand_shell(template: &str, shell: &str, cwd: &Path) -> String {
    let re = regex::Regex::new(r"!`([^`]+)`").unwrap();
    let mut out = String::new();
    let mut last = 0;
    for m in re.captures_iter(template) {
        let whole = m.get(0).unwrap();
        out.push_str(&template[last..whole.start()]);
        let cmd = &m[1];
        let result = tokio::process::Command::new(shell)
            .arg("-c")
            .arg(cmd)
            .current_dir(cwd)
            .output()
            .await;
        if let Ok(o) = result {
            out.push_str(&String::from_utf8_lossy(&o.stdout));
        }
        last = whole.end();
    }
    out.push_str(&template[last..]);
    out
}

/// Turn a template into prompt parts: the text plus `@file`/`@agent` references.
pub fn resolve_parts(engine: &Engine, template: &str) -> Vec<PartInput> {
    let mut parts = vec![PartInput::Text {
        id: None,
        text: template.to_string(),
        synthetic: false,
        ignored: false,
    }];
    let re = regex::Regex::new(markdown::FILE_REGEX).unwrap();
    let mut seen = std::collections::HashSet::new();
    let agents = engine.agents();
    for c in re.captures_iter(template) {
        let name = c[1].to_string();
        if name.is_empty() || !seen.insert(name.clone()) {
            continue;
        }
        let path = if name.starts_with("~/") {
            crate::paths::expand_home(&name, &engine.paths.home)
        } else {
            engine.project.worktree.join(&name)
        };
        match std::fs::metadata(&path) {
            Ok(meta) => parts.push(PartInput::File {
                id: None,
                url: format!("file://{}", path.display()),
                filename: Some(name.clone()),
                mime: if meta.is_dir() {
                    "application/x-directory".into()
                } else {
                    "text/plain".into()
                },
                source: None,
            }),
            Err(_) => {
                if let Some(a) = agents.get(&name) {
                    parts.push(PartInput::Agent {
                        id: None,
                        name: a.name.clone(),
                        source: None,
                    });
                }
            }
        }
    }
    parts
}

pub struct CommandInput {
    pub session_id: String,
    pub command: String,
    pub arguments: String,
    pub agent: Option<String>,
    pub model: Option<ModelRef>,
    pub variant: Option<String>,
    pub parts: Vec<PartInput>,
}

/// Execute a slash command: expand its template into a prompt (or subtask).
pub async fn execute(engine: Arc<Engine>, input: CommandInput) -> anyhow::Result<UserMessage> {
    let commands = engine.commands();
    let Some(cmd) = commands.get(&input.command).cloned() else {
        let available: Vec<&str> = commands.keys().map(String::as_str).collect();
        anyhow::bail!(
            "Command not found: \"{}\". Available commands: {}",
            input.command,
            available.join(", ")
        );
    };
    let mut template = substitute(&cmd.template, &input.arguments);
    if template.contains("!`") {
        let shell = crate::tool::builtins::bash::select_shell(engine.config().shell.as_deref());
        template = expand_shell(&template, &shell, &engine.directory).await;
    }
    let template = template.trim().to_string();

    let agents = engine.agents();
    let agent_name = cmd.agent.clone().or_else(|| input.agent.clone());
    let agent = match &agent_name {
        Some(n) => agents
            .get(n)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Agent not found: \"{n}\""))?,
        None => agents
            .default_agent(engine.config().default_agent.as_deref())
            .clone(),
    };
    let task_model: Option<ModelRef> = cmd
        .model
        .as_ref()
        .and_then(|m| {
            m.split_once('/').map(|(p, id)| ModelRef {
                provider_id: p.into(),
                model_id: id.into(),
                variant: None,
            })
        })
        .or_else(|| agent.model.clone())
        .or_else(|| input.model.clone());

    let template_parts = resolve_parts(&engine, &template);
    let input_files: std::collections::HashSet<String> = input
        .parts
        .iter()
        .filter_map(|p| match p {
            PartInput::File { url, .. } => Some(url.clone()),
            _ => None,
        })
        .collect();
    let is_subtask =
        (agent.mode == AgentMode::Subagent && cmd.subtask != Some(false)) || cmd.subtask == Some(true);
    let (parts, user_agent, user_model) = if is_subtask {
        let prompt = template_parts
            .iter()
            .find_map(|p| match p {
                PartInput::Text { text, .. } => Some(text.clone()),
                _ => None,
            })
            .unwrap_or_default();
        (
            vec![PartInput::Subtask {
                id: None,
                prompt,
                description: cmd.description.clone().unwrap_or_default(),
                agent: agent.name.clone(),
                command: Some(input.command.clone()),
                model: task_model.as_ref().map(|m| SubtaskModel {
                    provider_id: m.provider_id.clone(),
                    model_id: m.model_id.clone(),
                }),
            }],
            input.agent.clone(),
            input.model.clone(),
        )
    } else {
        let mut parts: Vec<PartInput> = template_parts
            .into_iter()
            .filter(|p| !matches!(p, PartInput::File { url, .. } if input_files.contains(url)))
            .collect();
        parts.extend(input.parts.clone());
        (parts, Some(agent.name.clone()), task_model)
    };
    let req = PromptRequest {
        model: user_model,
        agent: user_agent,
        variant: input.variant,
        parts,
        ..Default::default()
    };
    Ok(crate::session::runner::prompt(engine, &input.session_id, req).await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argument_substitution() {
        assert_eq!(
            substitute("fix $1 in $2", "bug \"my file.rs\""),
            "fix bug in my file.rs"
        );
        assert_eq!(substitute("do $ARGUMENTS now", "a b"), "do a b now");
        assert_eq!(substitute("plain", "extra"), "plain\n\nextra");
        assert_eq!(substitute("$1 then $2", "a b c d"), "a then b c d");
    }
}
