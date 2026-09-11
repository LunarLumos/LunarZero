//! `skill` — load a SKILL.md body into the conversation.

use std::borrow::Cow;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::tool::{Tool, ToolCtx, ToolError, ToolResult, parse_args};

#[derive(Deserialize)]
struct Args {
    name: String,
}

pub struct SkillTool;

#[async_trait]
impl Tool for SkillTool {
    fn id(&self) -> &'static str {
        "skill"
    }
    fn description(&self) -> Cow<'static, str> {
        Cow::Borrowed(crate::tool_description!("skill"))
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": { "name": { "type": "string", "description": "Skill name" } },
            "required": ["name"]
        })
    }
    async fn execute(&self, ctx: ToolCtx, args: Value) -> Result<ToolResult, ToolError> {
        let args: Args = parse_args(args)?;
        let skills = ctx.engine.skills();
        let Some(info) = skills.get(&args.name) else {
            let available: Vec<&str> = skills.keys().map(String::as_str).collect();
            return Err(ToolError::Invalid(format!(
                "Skill \"{}\" not found. Available skills: {}",
                args.name,
                if available.is_empty() {
                    "none".to_string()
                } else {
                    available.join(", ")
                }
            )));
        };
        ctx.ask(
            "skill",
            vec![args.name.clone()],
            vec![args.name.clone()],
            Default::default(),
        )
        .await?;
        let dir = info
            .location
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_default();
        let mut files: Vec<String> = Vec::new();
        let walker = ignore::WalkBuilder::new(&dir)
            .hidden(false)
            .follow_links(false)
            .build();
        for e in walker.flatten() {
            let p = e.path();
            if p.is_file() && p.file_name().and_then(|n| n.to_str()) != Some("SKILL.md") {
                files.push(p.display().to_string());
                if files.len() >= 10 {
                    break;
                }
            }
        }
        let output = format!(
            "<skill_content name=\"{n}\">\n# Skill: {n}\n\n{body}\n\nBase directory for this skill: {dir}\nRelative paths in this skill (e.g., scripts/, reference/) are relative to this base directory.\nNote: file list is sampled.\n\n<skill_files>\n{files}\n</skill_files>\n</skill_content>",
            n = info.name,
            body = info.content.trim(),
            dir = dir.display(),
            files = files
                .iter()
                .map(|f| format!("<file>{f}</file>"))
                .collect::<Vec<_>>()
                .join("\n")
        );
        Ok(ToolResult {
            title: format!("Loaded skill: {}", info.name),
            output,
            metadata: json!({ "name": info.name, "dir": dir.display().to_string() }),
            attachments: Vec::new(),
        })
    }
}
