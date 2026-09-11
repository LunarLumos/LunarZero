//! Skill discovery: `SKILL.md` files with `name`/`description` frontmatter
//! from Claude/agents dirs, config dirs and `skills.paths`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use lz_schema::api::SkillInfo;
use lz_schema::config::Config;
use lz_schema::permission::Action;

use crate::config::markdown;
use crate::paths::{Paths, env_var, expand_home};

#[derive(Debug, Clone)]
pub struct Skill {
    pub name: String,
    pub description: Option<String>,
    pub location: PathBuf,
    pub content: String,
}

impl Skill {
    pub fn to_info(&self) -> SkillInfo {
        SkillInfo {
            name: self.name.clone(),
            description: self.description.clone().unwrap_or_default(),
            location: self.location.display().to_string(),
        }
    }
}

fn scan(root: &Path, under: Option<&str>, out: &mut Vec<PathBuf>) {
    let base = match under {
        Some(u) => root.join(u),
        None => root.to_path_buf(),
    };
    if !base.is_dir() {
        return;
    }
    let walker = ignore::WalkBuilder::new(&base)
        .hidden(false)
        .git_ignore(false)
        .follow_links(true)
        .max_depth(Some(8))
        .build();
    for e in walker.flatten() {
        let p = e.path();
        if p.is_file() && p.file_name().and_then(|n| n.to_str()) == Some("SKILL.md") {
            out.push(p.to_path_buf());
        }
    }
}

pub fn discover(
    paths: &Paths,
    config: &Config,
    config_dirs: &[PathBuf],
    directory: &Path,
    worktree: &Path,
) -> BTreeMap<String, Skill> {
    let mut matches: Vec<PathBuf> = Vec::new();
    let disable_external = env_var("DISABLE_EXTERNAL_SKILLS").is_some();
    let disable_claude =
        env_var("DISABLE_CLAUDE_CODE").is_some() || env_var("DISABLE_CLAUDE_CODE_SKILLS").is_some();
    if !disable_external {
        let mut external = Vec::new();
        if !disable_claude {
            external.push(".claude");
        }
        external.push(".agents");
        for d in &external {
            scan(&paths.home.join(d), Some("skills"), &mut matches);
        }
        for dir in crate::config::ancestors(directory, Some(worktree)) {
            for d in &external {
                scan(&dir.join(d), Some("skills"), &mut matches);
            }
        }
    }
    for dir in config_dirs {
        scan(dir, Some("skill"), &mut matches);
        scan(dir, Some("skills"), &mut matches);
    }
    for item in config
        .skills
        .as_ref()
        .and_then(|s| s.paths.clone())
        .unwrap_or_default()
    {
        let expanded = expand_home(&item, &paths.home);
        let dir = if expanded.is_absolute() {
            expanded
        } else {
            directory.join(expanded)
        };
        scan(&dir, None, &mut matches);
    }
    let mut out = BTreeMap::new();
    for m in matches {
        let Ok(text) = std::fs::read_to_string(&m) else {
            continue;
        };
        let Ok(md) = markdown::parse(&text) else { continue };
        let Some(name) = md.data.get("name").and_then(|v| v.as_str()) else {
            continue;
        };
        let description = md
            .data
            .get("description")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        out.insert(
            name.to_string(),
            Skill {
                name: name.into(),
                description,
                location: m,
                content: md.content,
            },
        );
    }
    out
}

fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Skills this agent may load, sorted by name.
pub fn available<'a>(skills: &'a BTreeMap<String, Skill>, agent: &crate::agent::Agent) -> Vec<&'a Skill> {
    skills
        .values()
        .filter(|s| {
            crate::permission::evaluate("skill", &s.name, &[&agent.permission]).action != Action::Deny
        })
        .collect()
}

/// System-prompt block listing available skills.
pub fn format(list: &[&Skill], verbose: bool) -> String {
    let described: Vec<&&Skill> = list.iter().filter(|s| s.description.is_some()).collect();
    if described.is_empty() {
        return "No skills are currently available.".into();
    }
    if verbose {
        let mut lines = vec!["<available_skills>".to_string()];
        for s in described {
            lines.push("  <skill>".into());
            lines.push(format!("    <name>{}</name>", s.name));
            lines.push(format!(
                "    <description>{}</description>",
                s.description.as_deref().unwrap_or("")
            ));
            lines.push(format!(
                "    <location>{}</location>",
                escape_html(&s.location.display().to_string())
            ));
            lines.push("  </skill>".into());
        }
        lines.push("</available_skills>".into());
        return lines.join("\n");
    }
    let mut lines = vec!["## Available Skills".to_string()];
    for s in described {
        lines.push(format!(
            "- **{}**: {}",
            s.name,
            s.description.as_deref().unwrap_or("")
        ));
    }
    lines.join("\n")
}
