//! Tool-output truncation: oversized output is written to a file under the
//! data dir and replaced by a preview plus a hint.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use lz_schema::ids::{self, Prefix};

pub const MAX_LINES: usize = 2000;
pub const MAX_BYTES: usize = 50 * 1024;
const RETENTION: Duration = Duration::from_secs(7 * 24 * 60 * 60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Head,
    Tail,
}

pub struct Truncated {
    pub content: String,
    pub truncated: bool,
    pub output_path: Option<PathBuf>,
}

pub fn write(dir: &Path, text: &str) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let file = dir.join(ids::ascending(Prefix::Tool));
    std::fs::write(&file, text)?;
    Ok(file)
}

pub fn cleanup(dir: &Path) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let cutoff = SystemTime::now() - RETENTION;
    for e in rd.flatten() {
        let p = e.path();
        if !p
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("tool_"))
        {
            continue;
        }
        if let Ok(m) = e.metadata()
            && m.modified().is_ok_and(|t| t < cutoff)
        {
            let _ = std::fs::remove_file(&p);
        }
    }
}

pub fn output(
    dir: &Path,
    text: &str,
    max_lines: usize,
    max_bytes: usize,
    direction: Direction,
    has_task_tool: bool,
) -> Truncated {
    let lines: Vec<&str> = text.split('\n').collect();
    let total_bytes = text.len();
    if lines.len() <= max_lines && total_bytes <= max_bytes {
        return Truncated {
            content: text.to_string(),
            truncated: false,
            output_path: None,
        };
    }
    let mut out: Vec<&str> = Vec::new();
    let mut bytes = 0usize;
    let mut hit_bytes = false;
    match direction {
        Direction::Head => {
            for (i, line) in lines.iter().enumerate().take(max_lines) {
                let size = line.len() + usize::from(i > 0);
                if bytes + size > max_bytes {
                    hit_bytes = true;
                    break;
                }
                out.push(line);
                bytes += size;
            }
        }
        Direction::Tail => {
            for line in lines.iter().rev() {
                if out.len() >= max_lines {
                    break;
                }
                let size = line.len() + usize::from(!out.is_empty());
                if bytes + size > max_bytes {
                    hit_bytes = true;
                    break;
                }
                out.insert(0, line);
                bytes += size;
            }
        }
    }
    let removed = if hit_bytes {
        total_bytes - bytes
    } else {
        lines.len() - out.len()
    };
    let unit = if hit_bytes { "bytes" } else { "lines" };
    let preview = out.join("\n");
    let file = write(dir, text).ok();
    let path_str = file
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "<unavailable>".into());
    let hint = if has_task_tool {
        format!(
            "The tool call succeeded but the output was truncated. Full output saved to: {path_str}\nUse the Task tool to have explore agent process this file with Grep and Read (with offset/limit). Do NOT read the full file yourself - delegate to save context."
        )
    } else {
        format!(
            "The tool call succeeded but the output was truncated. Full output saved to: {path_str}\nUse Grep to search the full output or Read with offset/limit to view specific sections."
        )
    };
    let content = match direction {
        Direction::Head => format!("{preview}\n\n...{removed} {unit} truncated...\n\n{hint}"),
        Direction::Tail => format!("...{removed} {unit} truncated...\n\n{preview}\n\n{hint}"),
    };
    Truncated {
        content,
        truncated: true,
        output_path: file,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncates_by_lines() {
        let dir = tempfile::tempdir().unwrap();
        let text: String = (0..10).map(|i| format!("line{i}")).collect::<Vec<_>>().join("\n");
        let t = output(dir.path(), &text, 3, 10_000, Direction::Head, false);
        assert!(t.truncated);
        assert!(
            t.content
                .starts_with("line0\nline1\nline2\n\n...7 lines truncated...")
        );
        assert!(t.output_path.unwrap().exists());
        let t = output(dir.path(), &text, 3, 10_000, Direction::Tail, false);
        assert!(t.content.contains("line7\nline8\nline9"));
    }
}
