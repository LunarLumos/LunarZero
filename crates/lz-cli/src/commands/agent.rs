//! `lz agent list|create`.

use lz_schema::EngineApi;

use crate::cli::AgentCommand;
use crate::commands::config::resolve_dir;

pub async fn run(cmd: AgentCommand) -> anyhow::Result<i32> {
    let directory = resolve_dir(&None)?;
    let engine = lz_core::Engine::start(lz_core::EngineOptions {
        directory: directory.clone(),
        auto_approve: false,
        offline: true,
    })
    .await?;
    match cmd {
        AgentCommand::List => {
            for a in EngineApi::agents(engine.as_ref()).await? {
                if a.hidden {
                    continue;
                }
                let mode = format!("{:?}", a.mode).to_lowercase();
                let kind = if a.builtin { "builtin" } else { "custom" };
                println!(
                    "{:<14} {:<9} {:<8} {}",
                    a.name,
                    mode,
                    kind,
                    a.description.unwrap_or_default()
                );
            }
            Ok(0)
        }
        AgentCommand::Create {
            name,
            description,
            global,
        } => {
            let dir = if global {
                engine.paths.config.join("agent")
            } else {
                engine.project.worktree.join(".lunarzero").join("agent")
            };
            std::fs::create_dir_all(&dir)?;
            let path = dir.join(format!("{name}.md"));
            if path.exists() {
                anyhow::bail!("{} already exists", path.display());
            }
            let description = description.unwrap_or_else(|| format!("{name} agent"));
            std::fs::write(
                &path,
                format!(
                    "---\ndescription: {description}\nmode: subagent\n---\nYou are the {name} agent. Describe your role and rules here.\n"
                ),
            )?;
            println!("created {}", path.display());
            Ok(0)
        }
    }
}
