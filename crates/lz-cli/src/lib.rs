//! `lz` command-line entry point.

mod cli;
mod commands;
mod logging;

pub fn main_entry() {
    let code = match cli::run() {
        Ok(code) => code,
        Err(err) => {
            eprintln!("error: {err:#}");
            1
        }
    };
    std::process::exit(code);
}
