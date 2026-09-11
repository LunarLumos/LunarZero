//! `lz skill list|install|remove|update`.

use lz_core::skill_install::{self, Source};

use crate::cli::SkillCommand;
use crate::commands::config::resolve_dir;

async fn engine() -> anyhow::Result<std::sync::Arc<lz_core::Engine>> {
    let directory = resolve_dir(&None)?;
    lz_core::Engine::start(lz_core::EngineOptions {
        directory,
        auto_approve: false,
        offline: true,
    })
    .await
}

pub async fn run(cmd: SkillCommand) -> anyhow::Result<i32> {
    let engine = engine().await?;
    let paths = engine.paths.clone();
    let worktree = engine.project.worktree.clone();
    let code = match cmd {
        SkillCommand::List => {
            let skills = engine.skills();
            if skills.is_empty() {
                println!("no skills found. Install one: lz skill install owner/repo");
            }
            for s in skills.values() {
                let installed = s
                    .location
                    .parent()
                    .is_some_and(|d| d.join(skill_install::META_FILE).exists());
                println!(
                    "{:<28} {}{}",
                    s.name,
                    s.location.display(),
                    if installed { "  (installed)" } else { "" }
                );
                if let Some(d) = &s.description {
                    println!("{:<28} {}", "", d.chars().take(100).collect::<String>());
                }
            }
            0
        }
        SkillCommand::Install { source, project } => {
            let src = Source::parse(&source).map_err(|e| anyhow::anyhow!(e))?;
            let target = skill_install::target_dir(&paths, &worktree, !project);
            println!(
                "cloning {}{}…",
                src.url,
                src.subpath
                    .as_ref()
                    .map(|p| format!(" ({p})"))
                    .unwrap_or_default()
            );
            let list = skill_install::install(&src, &target)
                .await
                .map_err(|e| anyhow::anyhow!(e))?;
            for i in &list {
                println!("installed {:<24} → {}", i.name, i.path.display());
                if !i.description.is_empty() {
                    println!(
                        "          {}",
                        i.description.chars().take(100).collect::<String>()
                    );
                }
            }
            println!(
                "{} skill(s) ready; use `/skill-name` or let the agent load them.",
                list.len()
            );
            0
        }
        SkillCommand::Remove { name } => {
            let dir = skill_install::remove(&paths, &worktree, &name).map_err(|e| anyhow::anyhow!(e))?;
            println!("removed {}", dir.display());
            0
        }
        SkillCommand::Update => {
            let mut n = 0;
            for global in [true, false] {
                let dir = skill_install::target_dir(&paths, &worktree, global);
                for (name, meta, _) in skill_install::installed(&dir) {
                    let src = Source {
                        url: meta.source.clone(),
                        git_ref: meta.git_ref.clone(),
                        subpath: meta.subpath.clone(),
                    };
                    match skill_install::install(&src, &dir).await {
                        Ok(_) => {
                            println!("updated {name}");
                            n += 1;
                        }
                        Err(e) => eprintln!("{name}: {e}"),
                    }
                }
            }
            println!("{n} skill(s) updated");
            0
        }
    };
    engine.shutdown().await;
    Ok(code)
}
