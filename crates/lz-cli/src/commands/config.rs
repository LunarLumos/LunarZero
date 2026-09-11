use std::path::PathBuf;

use crate::cli::{ConfigCommand, TuiArgs};

pub fn resolve_dir(project: &Option<PathBuf>) -> anyhow::Result<PathBuf> {
    let dir = match project {
        Some(p) => p.clone(),
        None => std::env::current_dir()?,
    };
    Ok(dir.canonicalize().unwrap_or(dir))
}

pub async fn run(cmd: ConfigCommand, tui: &TuiArgs) -> anyhow::Result<i32> {
    let paths = lz_core::Paths::detect();
    let directory = resolve_dir(&tui.project)?;
    let project = lz_core::project::resolve(&directory);
    let loaded = lz_core::config::load(lz_core::config::LoadInput {
        paths: &paths,
        directory: &directory,
        worktree: &project.worktree,
    })?;
    match cmd {
        ConfigCommand::Show => {
            println!("{}", serde_json::to_string_pretty(&loaded.raw)?);
        }
        ConfigCommand::Path => {
            println!("config dir:   {}", paths.config.display());
            println!("data dir:     {}", paths.data.display());
            println!("cache dir:    {}", paths.cache.display());
            println!("state dir:    {}", paths.state.display());
            println!("project id:   {}", project.id);
            println!("worktree:     {}", project.worktree.display());
            println!("sources:");
            for s in &loaded.sources {
                println!("  {}", s.display());
            }
            println!("directories:");
            for d in &loaded.directories {
                println!("  {}", d.display());
            }
        }
        ConfigCommand::Schema => {
            println!(
                "{}",
                serde_json::to_string_pretty(&lz_core::config::json_schema())?
            );
        }
    }
    Ok(0)
}
