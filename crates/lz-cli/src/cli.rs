use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Parser, Debug)]
#[command(name = "lz", version = VERSION, about = "LunarZero — the fast terminal coding agent", long_about = None)]
pub struct Cli {
    #[command(flatten)]
    pub global: GlobalArgs,

    #[command(flatten)]
    pub tui: TuiArgs,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Args, Debug, Clone)]
pub struct GlobalArgs {
    /// Print logs to stderr as well as the log file
    #[arg(long, global = true)]
    pub print_logs: bool,
    /// Log level: trace, debug, info, warn, error
    #[arg(long, global = true, env = "LZ_LOG_LEVEL")]
    pub log_level: Option<String>,
}

/// Arguments for the default (TUI) command.
#[derive(Args, Debug, Clone, Default)]
pub struct TuiArgs {
    /// Project directory to open
    pub project: Option<PathBuf>,
    /// Model in `provider/model` form
    #[arg(short, long)]
    pub model: Option<String>,
    /// Continue the most recent session
    #[arg(short = 'c', long = "continue")]
    pub continue_session: bool,
    /// Session id to resume
    #[arg(short, long)]
    pub session: Option<String>,
    /// Fork the resumed session instead of continuing it
    #[arg(long)]
    pub fork: bool,
    /// Initial prompt to submit
    #[arg(long)]
    pub prompt: Option<String>,
    /// Agent to use
    #[arg(long)]
    pub agent: Option<String>,
    /// Auto-approve every permission request
    #[arg(long, visible_alias = "yolo", alias = "dangerously-skip-permissions")]
    pub auto: bool,
    /// Start in a permission mode: manual | accept-edits | auto | plan (shift+tab cycles in the TUI)
    #[arg(long, value_name = "MODE")]
    pub mode: Option<String>,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Run a prompt non-interactively
    Run(RunArgs),
    /// Manage provider credentials
    Auth {
        #[command(subcommand)]
        cmd: AuthCommand,
    },
    /// List available models
    Models {
        /// Only this provider
        provider: Option<String>,
        /// Refresh the models catalog
        #[arg(long)]
        refresh: bool,
    },
    /// Manage agents
    Agent {
        #[command(subcommand)]
        cmd: AgentCommand,
    },
    /// Guided first-run: connect a provider and verify it
    Setup,
    /// Curated MCP servers and skills you can install by alias
    Recommend,
    /// Skills: list, install from GitHub, remove, update
    Skill {
        #[command(subcommand)]
        cmd: SkillCommand,
    },
    /// Manage MCP servers
    Mcp {
        #[command(subcommand)]
        cmd: McpCommand,
    },
    /// Manage sessions
    Session {
        #[command(subcommand)]
        cmd: SessionCommand,
    },
    /// Export a session as JSON
    Export {
        session: String,
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Import a session from JSON
    Import { file: PathBuf },
    /// Show or inspect configuration
    Config {
        #[command(subcommand)]
        cmd: ConfigCommand,
    },
    /// The free-tier model pool behind `lunar/auto`: setup, models, usage
    Pool {
        #[command(subcommand)]
        cmd: Option<PoolCommand>,
    },
    /// Open the local web dashboard (API keys, free pool, sessions, config)
    Web {
        /// Port to listen on (127.0.0.1 only; 0 = random)
        #[arg(long, default_value_t = 7411)]
        port: u16,
        /// Don't open the browser automatically
        #[arg(long)]
        no_open: bool,
    },
    /// Generate shell completions
    Completion { shell: clap_complete::Shell },
    /// Upgrade to the latest release (or a specific version)
    Upgrade {
        /// Version tag to install, e.g. `v0.2.0` (default: latest)
        version: Option<String>,
        /// GitHub `owner/repo` to fetch releases from
        #[arg(long, env = "LZ_UPGRADE_REPO", default_value = "lunarzero/lunarzero")]
        repo: String,
        /// Only check, don't install
        #[arg(long)]
        check: bool,
    },
}

#[derive(Args, Debug, Clone)]
pub struct RunArgs {
    /// The prompt (also read from stdin when piped)
    pub message: Vec<String>,
    /// Output format
    #[arg(long, default_value = "text")]
    pub format: RunFormat,
    /// Attach files to the prompt
    #[arg(long)]
    pub file: Vec<PathBuf>,
    #[arg(short = 'c', long = "continue")]
    pub continue_session: bool,
    #[arg(short, long)]
    pub session: Option<String>,
    #[arg(long)]
    pub fork: bool,
    #[arg(long)]
    pub agent: Option<String>,
    #[arg(short, long)]
    pub model: Option<String>,
    #[arg(long)]
    pub variant: Option<String>,
    /// Run a slash command instead of a plain prompt
    #[arg(long)]
    pub command: Option<String>,
    /// Session title
    #[arg(long)]
    pub title: Option<String>,
    /// Auto-approve every permission request
    #[arg(long, visible_alias = "yolo", alias = "dangerously-skip-permissions")]
    pub auto: bool,
    /// Working directory
    #[arg(long)]
    pub dir: Option<PathBuf>,
}

#[derive(clap::ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunFormat {
    Text,
    Json,
}

#[derive(Subcommand, Debug)]
pub enum PoolCommand {
    /// Providers in the pool, which ones have keys, and where to get a key
    Setup,
    /// Pool models with rank, speed and limits (connected ones marked)
    List {
        /// Include models of providers without a key
        #[arg(long)]
        all: bool,
    },
    /// Today's usage and cooldowns per pool model
    Status,
}

#[derive(Subcommand, Debug)]
pub enum AuthCommand {
    /// List stored credentials
    List,
    /// Store an API key for a provider
    Login {
        provider: Option<String>,
        /// API key (prompted when omitted)
        #[arg(long)]
        key: Option<String>,
    },
    /// Remove a stored credential
    Logout { provider: Option<String> },
}

#[derive(Subcommand, Debug)]
pub enum SkillCommand {
    /// Skills visible in this project (with where they come from)
    List,
    /// Install from `owner/repo`, a GitHub URL (optionally to a sub-folder), or any git URL
    Install {
        source: String,
        /// Install for this project only (`.lunarzero/skills/`) instead of globally
        #[arg(long)]
        project: bool,
    },
    /// Remove an installed skill
    Remove { name: String },
    /// Re-install every skill that was installed from a repository
    Update,
}

#[derive(Subcommand, Debug)]
pub enum AgentCommand {
    List,
    /// Create a new agent definition file
    Create {
        name: String,
        #[arg(long)]
        description: Option<String>,
        #[arg(long)]
        global: bool,
    },
}

#[derive(Subcommand, Debug)]
pub enum McpCommand {
    List,
    /// Install from GitHub (`owner/repo`, link, sub-folder), `npm:<package>` or `pypi:<package>`, then connect
    Install {
        source: String,
        /// Config entry name (default: derived from the source)
        #[arg(long)]
        name: Option<String>,
        /// Register in the global config instead of this project
        #[arg(long)]
        global: bool,
    },
    Add {
        name: String,
        /// Command for a local (stdio) server
        #[arg(long, num_args = 1.., allow_hyphen_values = true)]
        command: Vec<String>,
        /// URL for a remote server
        #[arg(long)]
        url: Option<String>,
        #[arg(long)]
        global: bool,
    },
}

#[derive(Subcommand, Debug)]
pub enum SessionCommand {
    List {
        #[arg(long, default_value_t = 20)]
        limit: usize,
        #[arg(long)]
        json: bool,
    },
    Delete {
        id: String,
    },
}

#[derive(Subcommand, Debug)]
pub enum ConfigCommand {
    /// Print the merged configuration as JSON
    Show,
    /// Print the config files and directories that were loaded
    Path,
    /// Print the JSON schema for lunarzero.json
    Schema,
}

pub fn run() -> anyhow::Result<i32> {
    let cli = Cli::parse();
    logging::init(&cli.global);
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    rt.block_on(commands::dispatch(cli))
}

use crate::commands;
use crate::logging;
