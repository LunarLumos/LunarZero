mod agent;
mod auth;
mod config;
mod mcp;
mod models;
mod pool;
mod run;
mod session;
mod skill;
mod tui;
mod upgrade;
mod web;

use crate::cli::{Cli, Command};

pub async fn dispatch(cli: Cli) -> anyhow::Result<i32> {
    match cli.command {
        Some(Command::Run(args)) => run::exec(args).await,
        Some(Command::Auth { cmd }) => auth::run(cmd).await,
        Some(Command::Agent { cmd }) => agent::run(cmd).await,
        Some(Command::Skill { cmd }) => skill::run(cmd).await,
        Some(Command::Recommend) => {
            let r = lz_core::recommended::catalog();
            println!("Built-in skills (always available, loaded on demand):");
            for (name, text) in lz_core::skill::BUILTIN {
                let desc = text
                    .lines()
                    .find_map(|l| l.strip_prefix("description: "))
                    .unwrap_or("");
                println!("  {name:<20} {desc}");
            }
            println!("\nMCP servers — install with `lz mcp install <alias>` (or /install --mcp <alias>):");
            for m in &r.mcp {
                println!(
                    "  {:<20} {}{}",
                    m.alias,
                    m.description,
                    if m.env.is_empty() {
                        String::new()
                    } else {
                        format!("  [env: {}]", m.env.join(", "))
                    }
                );
            }
            println!("\nSkills — install with `lz skill install <alias>`:");
            for s in &r.skills {
                println!("  {:<20} {}", s.alias, s.description);
            }
            println!("\nEach MCP server adds its tools to every request; install the ones you use.");
            Ok(0)
        }
        Some(Command::Models { provider, refresh }) => models::run(provider, refresh).await,
        Some(Command::Session { cmd }) => session::run(cmd).await,
        Some(Command::Export { session, output }) => session::export(session, output).await,
        Some(Command::Import { file }) => session::import(file).await,
        Some(Command::Config { cmd }) => config::run(cmd, &cli.tui).await,
        Some(Command::Completion { shell }) => {
            use clap::CommandFactory;
            let mut cmd = Cli::command();
            clap_complete::generate(shell, &mut cmd, "lz", &mut std::io::stdout());
            Ok(0)
        }
        Some(Command::Mcp { cmd }) => mcp::run(cmd).await,
        Some(Command::Pool { cmd }) => pool::run(cmd).await,
        Some(Command::Web { port, no_open }) => web::exec(port, no_open).await,
        Some(Command::Upgrade { version, repo, check }) => upgrade::run(version, repo, check).await,
        None => tui::exec(cli.tui).await,
    }
}
