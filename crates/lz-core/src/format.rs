//! Post-edit formatting: after the agent writes a file, run the project's own
//! formatter on it so the diff the user reviews (and CI lints) never fights
//! over whitespace. Formatters are detected, never installed: rustfmt,
//! prettier/biome from `node_modules`, gofmt, ruff/black (venv first), zig
//! fmt, mix format. `formatter` in config can turn it off or add entries.

use std::path::{Path, PathBuf};
use std::time::Duration;

use lz_schema::config::FormatterConfig;
use serde_json::Value;

const TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone)]
pub struct Formatter {
    pub name: String,
    /// argv; `$FILE` is replaced by the path
    pub command: Vec<String>,
    pub extensions: Vec<String>,
}

fn on_path(bin: &str) -> bool {
    crate::lsp::servers::on_path(bin)
}

fn exists_in(worktree: &Path, rel: &str) -> Option<PathBuf> {
    let p = worktree.join(rel);
    p.is_file().then_some(p)
}

fn s(v: &[&str]) -> Vec<String> {
    v.iter().map(|x| x.to_string()).collect()
}

/// Formatters that apply to this worktree.
pub fn detect(worktree: &Path, config: Option<&FormatterConfig>) -> Vec<Formatter> {
    let mut out = Vec::new();
    match config {
        Some(FormatterConfig::Enabled(false)) => return out,
        Some(FormatterConfig::Entries(map)) => {
            for (name, v) in map {
                if v == &Value::Bool(false) {
                    continue;
                }
                let command: Vec<String> = v["command"]
                    .as_array()
                    .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
                    .unwrap_or_default();
                let extensions: Vec<String> = v["extensions"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|x| x.as_str().map(|e| e.trim_start_matches('.').to_string()))
                            .collect()
                    })
                    .unwrap_or_default();
                if !command.is_empty() && !extensions.is_empty() {
                    out.push(Formatter {
                        name: name.clone(),
                        command,
                        extensions,
                    });
                }
            }
        }
        _ => {}
    }
    let disabled: Vec<&str> = match config {
        Some(FormatterConfig::Entries(map)) => map
            .iter()
            .filter(|(_, v)| *v == &Value::Bool(false))
            .map(|(k, _)| k.as_str())
            .collect(),
        _ => Vec::new(),
    };
    let mut add = |name: &str, command: Vec<String>, exts: &[&str]| {
        if disabled.contains(&name) || out.iter().any(|f| f.name == name) {
            return;
        }
        out.push(Formatter {
            name: name.into(),
            command,
            extensions: s(exts),
        });
    };

    if exists_in(worktree, "Cargo.toml").is_some() && on_path("rustfmt") {
        let edition = std::fs::read_to_string(worktree.join("Cargo.toml"))
            .ok()
            .and_then(|t| {
                t.lines().find_map(|l| {
                    let l = l.trim();
                    l.strip_prefix("edition")
                        .and_then(|r| r.split('=').nth(1))
                        .map(|v| v.trim().trim_matches('"').to_string())
                })
            })
            .unwrap_or_else(|| "2021".into());
        add(
            "rustfmt",
            vec!["rustfmt".into(), "--edition".into(), edition, "$FILE".into()],
            &["rs"],
        );
    }
    if let Some(biome) = exists_in(worktree, "node_modules/.bin/biome") {
        add(
            "biome",
            vec![
                biome.display().to_string(),
                "format".into(),
                "--write".into(),
                "$FILE".into(),
            ],
            &["js", "jsx", "ts", "tsx", "json", "jsonc", "css"],
        );
    } else if let Some(prettier) = exists_in(worktree, "node_modules/.bin/prettier") {
        add(
            "prettier",
            vec![
                prettier.display().to_string(),
                "--write".into(),
                "--log-level".into(),
                "silent".into(),
                "$FILE".into(),
            ],
            &[
                "js", "jsx", "mjs", "cjs", "ts", "tsx", "mts", "cts", "json", "css", "scss", "less", "html",
                "vue", "svelte", "md", "yaml", "yml", "graphql",
            ],
        );
    }
    if on_path("gofmt")
        && (exists_in(worktree, "go.mod").is_some() || exists_in(worktree, "go.work").is_some())
    {
        add("gofmt", s(&["gofmt", "-w", "$FILE"]), &["go"]);
    }
    let py_project = ["pyproject.toml", "setup.py", "requirements.txt", "Pipfile"]
        .iter()
        .any(|f| exists_in(worktree, f).is_some());
    if py_project {
        let venv_bin = |name: &str| {
            [".venv/bin", "venv/bin"]
                .iter()
                .find_map(|d| exists_in(worktree, &format!("{d}/{name}")))
                .map(|p| p.display().to_string())
        };
        if let Some(ruff) = venv_bin("ruff").or_else(|| on_path("ruff").then(|| "ruff".to_string())) {
            add(
                "ruff",
                vec![ruff, "format".into(), "-q".into(), "$FILE".into()],
                &["py", "pyi"],
            );
        } else if let Some(black) =
            venv_bin("black").or_else(|| on_path("black").then(|| "black".to_string()))
        {
            add("black", vec![black, "-q".into(), "$FILE".into()], &["py", "pyi"]);
        }
    }
    if exists_in(worktree, "build.zig").is_some() && on_path("zig") {
        add("zig", s(&["zig", "fmt", "$FILE"]), &["zig"]);
    }
    if exists_in(worktree, "mix.exs").is_some() && on_path("mix") {
        add("mix", s(&["mix", "format", "$FILE"]), &["ex", "exs"]);
    }
    if exists_in(worktree, "pubspec.yaml").is_some() && on_path("dart") {
        add("dart", s(&["dart", "format", "$FILE"]), &["dart"]);
    }
    out
}

/// Run the matching formatter on `path`. Returns the new content when the
/// file changed; `None` when nothing applies, nothing changed, or it failed
/// (a failing formatter must never undo the agent's edit).
///
/// `previous` is the file's content before the agent's edit: when that was
/// not formatter-clean already, the file is left alone — reformatting it now
/// would bury the agent's change in unrelated whitespace churn.
pub async fn run(
    worktree: &Path,
    formatters: &[Formatter],
    path: &Path,
    previous: Option<&str>,
) -> Option<String> {
    let ext = path.extension()?.to_str()?;
    let f = formatters
        .iter()
        .find(|f| f.extensions.iter().any(|e| e == ext))?;
    if let Some(prev) = previous.filter(|p| !p.trim().is_empty()) {
        let dir = path.parent()?;
        let probe = dir.join(format!(".lz-fmt-probe-{}.{ext}", std::process::id()));
        std::fs::write(&probe, prev).ok()?;
        let clean = format_in_place(worktree, f, &probe).await == Some(false);
        let _ = std::fs::remove_file(&probe);
        if !clean {
            tracing::debug!(file = %path.display(), "not formatter-clean before the edit; leaving as is");
            return None;
        }
    }
    let before = std::fs::read_to_string(path).ok()?;
    format_in_place(worktree, f, path).await?;
    let after = std::fs::read_to_string(path).ok()?;
    (after != before).then_some(after)
}

/// Run a formatter on one file; `Some(changed)` on success.
async fn format_in_place(worktree: &Path, f: &Formatter, path: &Path) -> Option<bool> {
    let before = std::fs::read_to_string(path).ok()?;
    let argv: Vec<String> = f
        .command
        .iter()
        .map(|a| a.replace("$FILE", &path.display().to_string()))
        .collect();
    let mut cmd = tokio::process::Command::new(&argv[0]);
    cmd.args(&argv[1..])
        .current_dir(worktree)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped());
    let child = cmd.spawn().ok()?;
    let out = match tokio::time::timeout(TIMEOUT, child.wait_with_output()).await {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => {
            tracing::debug!(formatter = f.name, "failed to run: {e}");
            return None;
        }
        Err(_) => {
            tracing::warn!(formatter = f.name, "timed out after {TIMEOUT:?}");
            return None;
        }
    };
    if !out.status.success() {
        // a syntax error the formatter refuses is left for the diagnostics/tests
        tracing::debug!(
            formatter = f.name,
            "exit {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
        return None;
    }
    let after = std::fs::read_to_string(path).ok()?;
    Some(after != before)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_rustfmt_for_cargo_projects() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"x\"\nedition = \"2024\"\n",
        )
        .unwrap();
        let f = detect(dir.path(), None);
        if on_path("rustfmt") {
            let r = f.iter().find(|f| f.name == "rustfmt").expect("rustfmt detected");
            assert!(r.command.contains(&"2024".to_string()));
        }
        assert!(detect(dir.path(), Some(&FormatterConfig::Enabled(false))).is_empty());
        let custom: FormatterConfig = serde_json::from_value(serde_json::json!({
            "rustfmt": false,
            "mine": { "command": ["myfmt", "$FILE"], "extensions": ["foo"] }
        }))
        .unwrap();
        let f = detect(dir.path(), Some(&custom));
        assert!(f.iter().any(|f| f.name == "mine"));
        assert!(!f.iter().any(|f| f.name == "rustfmt"));
    }

    #[tokio::test]
    async fn rustfmt_reformats_a_file() {
        if !on_path("rustfmt") {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"x\"\nedition = \"2021\"\n",
        )
        .unwrap();
        let file = dir.path().join("lib.rs");
        std::fs::write(&file, "fn  main( ){let x=1;println!(\"{}\",x);}\n").unwrap();
        let f = detect(dir.path(), None);
        // new file (no previous content): formatted
        let after = run(dir.path(), &f, &file, None).await.expect("reformatted");
        assert!(after.contains("fn main() {"), "{after}");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), after);
        // previously clean file: formatted
        std::fs::write(
            &file,
            "fn main() {\n    let x = 1;\n    println!(\"{}\", x);\n}\nfn  other( ){}\n",
        )
        .unwrap();
        let clean_prev = "fn main() {\n    let x = 1;\n    println!(\"{}\", x);\n}\n";
        assert!(run(dir.path(), &f, &file, Some(clean_prev)).await.is_some());
        // previously messy file: left alone
        std::fs::write(&file, "fn  a( ){}\nfn  b( ){}\n").unwrap();
        assert!(run(dir.path(), &f, &file, Some("fn  a( ){}\n")).await.is_none());
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "fn  a( ){}\nfn  b( ){}\n"
        );
    }
}
