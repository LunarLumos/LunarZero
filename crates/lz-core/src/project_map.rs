//! A compact map of the repository (languages, layout, commands, key files)
//! that rides in the system prompt so the model can orient itself without
//! spending tool calls — each of which re-sends the whole context.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::Value;

const TTL: Duration = Duration::from_secs(300);
const MAX_FILES: usize = 30_000;

#[derive(Default)]
pub struct Cache {
    inner: std::sync::Mutex<Option<(Instant, String)>>,
}

impl Cache {
    pub fn get(&self, worktree: &Path, max_chars: usize) -> String {
        let mut g = self.inner.lock().unwrap();
        if let Some((at, s)) = &*g
            && at.elapsed() < TTL
        {
            return s.clone();
        }
        let s = build(worktree, max_chars);
        *g = Some((Instant::now(), s.clone()));
        s
    }
    pub fn invalidate(&self) {
        *self.inner.lock().unwrap() = None;
    }
}

fn lang(ext: &str) -> Option<&'static str> {
    Some(match ext {
        "rs" => "rust",
        "ts" | "tsx" | "mts" | "cts" => "ts",
        "js" | "jsx" | "mjs" | "cjs" => "js",
        "py" => "python",
        "go" => "go",
        "java" => "java",
        "kt" | "kts" => "kotlin",
        "swift" => "swift",
        "c" | "h" => "c",
        "cc" | "cpp" | "cxx" | "hpp" | "hh" => "cpp",
        "cs" => "csharp",
        "rb" => "ruby",
        "php" => "php",
        "ex" | "exs" => "elixir",
        "dart" => "dart",
        "scala" => "scala",
        "sh" | "bash" | "zsh" => "shell",
        "sql" => "sql",
        "html" => "html",
        "css" | "scss" | "less" => "css",
        "md" | "mdx" => "md",
        "json" | "jsonc" => "json",
        "yaml" | "yml" => "yaml",
        "toml" => "toml",
        "proto" => "proto",
        "tf" => "terraform",
        _ => return None,
    })
}

/// Build the map text (already capped at `max_chars`).
pub fn build(worktree: &Path, max_chars: usize) -> String {
    let mut total = 0usize;
    let mut langs: BTreeMap<&'static str, usize> = BTreeMap::new();
    // top-level dir → (files, second-level dirs)
    let mut dirs: BTreeMap<String, (usize, BTreeMap<String, usize>)> = BTreeMap::new();
    let mut root_files: Vec<String> = Vec::new();
    let walker = ignore::WalkBuilder::new(worktree)
        .hidden(true)
        .git_ignore(true)
        .git_exclude(true)
        .max_depth(Some(6))
        .build();
    for e in walker.flatten() {
        let p = e.path();
        if !p.is_file() {
            continue;
        }
        total += 1;
        if total > MAX_FILES {
            break;
        }
        if let Some(l) = p.extension().and_then(|x| x.to_str()).and_then(lang) {
            *langs.entry(l).or_default() += 1;
        }
        let Ok(rel) = p.strip_prefix(worktree) else {
            continue;
        };
        let comps: Vec<String> = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy().to_string())
            .collect();
        match comps.len() {
            1 => root_files.push(comps[0].clone()),
            n => {
                let entry = dirs.entry(comps[0].clone()).or_default();
                entry.0 += 1;
                if n > 2 {
                    *entry.1.entry(comps[1].clone()).or_default() += 1;
                }
            }
        }
    }
    let mut out = String::from("<project_map>\n");
    // languages
    let mut lv: Vec<(&str, usize)> = langs.iter().map(|(k, v)| (*k, *v)).collect();
    lv.sort_by(|a, b| b.1.cmp(&a.1));
    let langs_s: Vec<String> = lv.iter().take(6).map(|(k, v)| format!("{k} {v}")).collect();
    out.push_str(&format!(
        "files: {total}{}\n",
        if langs_s.is_empty() {
            String::new()
        } else {
            format!(" · {}", langs_s.join(", "))
        }
    ));
    // commands from manifests
    let cmds = commands(worktree, &root_files);
    if !cmds.is_empty() {
        out.push_str(&format!("commands: {}\n", cmds.join(" · ")));
    }
    // tree, two levels
    out.push_str("tree:\n");
    let mut dv: Vec<(&String, &(usize, BTreeMap<String, usize>))> = dirs.iter().collect();
    dv.sort_by(|a, b| b.1.0.cmp(&a.1.0));
    for (d, (n, sub)) in dv.iter().take(18) {
        let mut subs: Vec<(&String, &usize)> = sub.iter().collect();
        subs.sort_by(|a, b| b.1.cmp(a.1));
        let subs_s: Vec<String> = subs.iter().take(8).map(|(s, _)| format!("{s}/")).collect();
        out.push_str(&format!(
            "  {d}/ ({n}){}{}\n",
            if subs_s.is_empty() { "" } else { " " },
            subs_s.join(" ")
        ));
    }
    // key root files
    let key: Vec<&String> = root_files
        .iter()
        .filter(|f| {
            let l = f.to_lowercase();
            l.starts_with("readme")
                || l == "agents.md"
                || l == "claude.md"
                || l == "contributing.md"
                || l.starts_with("package.json")
                || l == "cargo.toml"
                || l == "pyproject.toml"
                || l == "go.mod"
                || l == "makefile"
                || l == "justfile"
                || l == "dockerfile"
                || l.starts_with("docker-compose")
                || l.ends_with(".sln")
                || l == "pom.xml"
                || l == "build.gradle"
                || l == "build.gradle.kts"
                || l == "tsconfig.json"
                || l == "setup.py"
                || l == "requirements.txt"
        })
        .collect();
    if !key.is_empty() {
        out.push_str(&format!(
            "root: {}\n",
            key.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
        ));
    }
    if let Some(entries) = entry_points(worktree) {
        out.push_str(&format!("entry: {entries}\n"));
    }
    out.push_str("</project_map>");
    if out.len() > max_chars {
        let cut = out
            .char_indices()
            .nth(max_chars.saturating_sub(16))
            .map(|(i, _)| i)
            .unwrap_or(out.len());
        out.truncate(cut);
        out.push_str("…\n</project_map>");
    }
    out
}

fn commands(worktree: &Path, root_files: &[String]) -> Vec<String> {
    let has = |n: &str| root_files.iter().any(|f| f == n);
    let mut out = Vec::new();
    if has("package.json")
        && let Ok(text) = std::fs::read_to_string(worktree.join("package.json"))
        && let Ok(v) = serde_json::from_str::<Value>(&text)
    {
        let pm = if has("pnpm-lock.yaml") {
            "pnpm"
        } else if has("yarn.lock") {
            "yarn"
        } else if has("bun.lockb") {
            "bun"
        } else {
            "npm run"
        };
        if let Some(scripts) = v.get("scripts").and_then(Value::as_object) {
            for k in ["build", "test", "lint", "dev", "typecheck"] {
                if scripts.contains_key(k) {
                    out.push(format!("{k}: {pm} {k}"));
                }
            }
        }
    }
    if has("Cargo.toml") {
        out.push("build: cargo build".into());
        out.push("test: cargo test".into());
        out.push("lint: cargo clippy".into());
    }
    if has("pyproject.toml") || has("setup.py") {
        let runner = if has("uv.lock") { "uv run pytest" } else { "pytest" };
        out.push(format!("test: {runner}"));
    }
    if has("go.mod") {
        out.push("build: go build ./...".into());
        out.push("test: go test ./...".into());
    }
    if has("Makefile")
        && let Ok(text) = std::fs::read_to_string(worktree.join("Makefile"))
    {
        let targets: Vec<String> = text
            .lines()
            .filter_map(|l| {
                let (t, _) = l.split_once(':')?;
                let t = t.trim();
                (!t.is_empty()
                    && !t.starts_with('.')
                    && !t.starts_with('#')
                    && !t.contains(' ')
                    && !t.contains('=')
                    && !t.contains('$'))
                .then(|| t.to_string())
            })
            .take(8)
            .collect();
        if !targets.is_empty() {
            out.push(format!("make: {}", targets.join(" ")));
        }
    }
    out
}

fn entry_points(worktree: &Path) -> Option<String> {
    let candidates = [
        "src/main.rs",
        "src/lib.rs",
        "main.py",
        "app.py",
        "src/index.ts",
        "src/main.ts",
        "src/index.js",
        "index.js",
        "main.go",
        "cmd",
        "src/App.tsx",
        "app/page.tsx",
        "manage.py",
    ];
    let found: Vec<&str> = candidates
        .iter()
        .copied()
        .filter(|c| worktree.join(c).exists())
        .collect();
    if found.is_empty() {
        None
    } else {
        Some(found.join(", "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_this_repo() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap();
        let m = build(root, 1500);
        assert!(m.starts_with("<project_map>"));
        assert!(m.contains("rust"));
        assert!(m.contains("crates/"));
        assert!(m.contains("cargo test"));
        assert!(m.len() <= 1500 + 32);
    }
}
