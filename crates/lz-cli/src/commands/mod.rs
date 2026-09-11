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
