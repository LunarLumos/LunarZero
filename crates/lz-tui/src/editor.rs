//! `$EDITOR` round-trip for composing a prompt.

use std::path::PathBuf;

pub fn editor_command() -> Option<String> {
    std::env::var("VISUAL")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var("EDITOR").ok().filter(|s| !s.is_empty()))
}

/// Blocking: writes `initial` to a temp file, runs the editor attached to the
/// real terminal, and returns the edited text (None if unchanged/empty).
pub fn edit_blocking(initial: &str) -> anyhow::Result<Option<String>> {
    let Some(cmd) = editor_command() else {
        anyhow::bail!("$EDITOR is not set");
    };
    let path: PathBuf = std::env::temp_dir().join(format!("lz-prompt-{}.md", std::process::id()));
    std::fs::write(&path, initial)?;
    let words = shell_split(&cmd);
    let status = std::process::Command::new(&words[0])
        .args(&words[1..])
        .arg(&path)
        .status()?;
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let _ = std::fs::remove_file(&path);
    if !status.success() {
        anyhow::bail!("editor exited with {status}");
    }
    let trimmed = text.trim_end().to_string();
    Ok(if trimmed.is_empty() { None } else { Some(trimmed) })
}

fn shell_split(s: &str) -> Vec<String> {
    let v: Vec<String> = s.split_whitespace().map(str::to_string).collect();
    if v.is_empty() { vec!["vi".into()] } else { v }
}
