use tracing_subscriber::EnvFilter;

use crate::cli::GlobalArgs;

pub fn init(args: &GlobalArgs) {
    let level = args.log_level.clone().unwrap_or_else(|| "info".into());
    let filter = EnvFilter::try_new(format!("lz_core={level},lz_tui={level},lz_cli={level}"))
        .unwrap_or_else(|_| EnvFilter::new("info"));
    let paths = lz_core::Paths::detect();
    let _ = std::fs::create_dir_all(paths.log());
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(paths.log().join("lunarzero.log"))
        .ok();
    let builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(false)
        .with_target(true);
    if args.print_logs {
        builder.with_writer(std::io::stderr).init();
    } else if let Some(file) = file {
        builder.with_writer(std::sync::Mutex::new(file)).init();
    } else {
        builder.with_writer(std::io::sink).init();
    }
}
