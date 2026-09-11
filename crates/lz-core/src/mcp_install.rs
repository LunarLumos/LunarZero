//! Install MCP servers from source: a GitHub repo (cloned, dependencies
//! installed, built, launch command detected) or a package shortcut
//! (`npm:<pkg>` → `npx -y`, `pypi:<pkg>` → `uvx`). The result is a `local`
//! MCP config entry that `lz mcp add` would have needed by hand.

use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;

use crate::paths::Paths;
use crate::skill_install::Source;

#[derive(Debug, Clone, Serialize)]
pub struct McpInstalled {
    pub name: String,
    pub command: Vec<String>,
    pub cwd: Option<String>,
    pub runtime: String,
    /// where the checkout lives (none for package shortcuts)
    pub dir: Option<String>,
    pub notes: Vec<String>,
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn on_path(bin: &str) -> bool {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| d.join(bin).is_file()))
        .unwrap_or(false)
}

async fn run(cmd: &str, args: &[&str], cwd: &Path, log: &mut Vec<String>) -> Result<(), String> {
    log.push(format!("$ {cmd} {}", args.join(" ")));
    let out = tokio::process::Command::new(cmd)
        .args(args)
        .current_dir(cwd)
        .stdin(std::process::Stdio::null())
        .output()
        .await
        .map_err(|e| format!("{cmd}: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let tail: String = err
            .lines()
            .rev()
            .take(15)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join("\n");
        return Err(format!("`{cmd} {}` failed:\n{tail}", args.join(" ")));
    }
    Ok(())
}

/// Derive a config name from the source (`owner/repo` → `repo`, `@scope/pkg` → `pkg`).
pub fn default_name(source: &str, subpath: Option<&str>) -> String {
    let base = subpath
        .and_then(|p| p.rsplit('/').next())
        .map(str::to_string)
        .unwrap_or_else(|| {
            source
                .trim_end_matches('/')
                .trim_end_matches(".git")
                .rsplit(['/', ':'])
                .next()
                .unwrap_or("mcp")
                .to_string()
        });
    let base = base
        .strip_prefix("mcp-server-")
        .or_else(|| base.strip_prefix("server-"))
        .unwrap_or(&base);
    let base = base
        .strip_suffix("-mcp-server")
        .or_else(|| base.strip_suffix("-mcp"))
        .or_else(|| base.strip_suffix("-server"))
        .unwrap_or(base);
    let cleaned: String = base
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    if cleaned.is_empty() { "mcp".into() } else { cleaned }
}

/// Install from `source`; returns the launch description.
pub async fn install(paths: &Paths, source: &str, name: Option<String>) -> Result<McpInstalled, String> {
    let src = source.trim();
    if let Some(pkg) = src.strip_prefix("npm:") {
        if !on_path("npx") {
            return Err("npx (Node.js) is required for npm packages".into());
        }
        return Ok(McpInstalled {
            name: name.unwrap_or_else(|| default_name(pkg, None)),
            command: vec!["npx".into(), "-y".into(), pkg.into()],
            cwd: None,
            runtime: "npm".into(),
            dir: None,
            notes: vec![],
        });
    }
    if let Some(pkg) = src.strip_prefix("pypi:").or_else(|| src.strip_prefix("pip:")) {
        if !on_path("uvx") {
            return Err("uvx (https://docs.astral.sh/uv/) is required for PyPI packages".into());
        }
        return Ok(McpInstalled {
            name: name.unwrap_or_else(|| default_name(pkg, None)),
            command: vec!["uvx".into(), pkg.into()],
            cwd: None,
            runtime: "pypi".into(),
            dir: None,
            notes: vec![],
        });
    }
    let parsed = Source::parse(src)?;
    let name = name.unwrap_or_else(|| default_name(&parsed.url, parsed.subpath.as_deref()));
    let dir = paths.data.join("mcp").join(&name);
    let mut log = Vec::new();
    if dir.exists() {
        std::fs::remove_dir_all(&dir).map_err(|e| e.to_string())?;
    }
    std::fs::create_dir_all(dir.parent().unwrap()).map_err(|e| e.to_string())?;
    let mut args = vec!["clone", "--depth", "1", "--quiet"];
    if let Some(r) = &parsed.git_ref {
        args.extend(["--branch", r.as_str()]);
    }
    let dir_s = dir.display().to_string();
    args.push(&parsed.url);
    args.push(&dir_s);
    run("git", &args, paths.data.as_path(), &mut log).await?;
    let root = match &parsed.subpath {
        Some(p) => dir.join(p),
        None => dir.clone(),
    };
    if !root.is_dir() {
        return Err(format!(
            "`{}` does not exist in the repository",
            parsed.subpath.clone().unwrap_or_default()
        ));
    }
    let (runtime, command, cwd) = detect_and_build(&root, &mut log).await?;
    let _ = std::fs::write(dir.join(".lz-mcp.json"), serde_json::json!({ "source": parsed.url, "git_ref": parsed.git_ref, "subpath": parsed.subpath, "installed_at": now_ms() }).to_string());
    Ok(McpInstalled {
        name,
        command,
        cwd: Some(cwd.display().to_string()),
        runtime,
        dir: Some(dir.display().to_string()),
        notes: log,
    })
}

/// Look at the project files, build, and return (runtime, command, cwd).
async fn detect_and_build(
    root: &Path,
    log: &mut Vec<String>,
) -> Result<(String, Vec<String>, PathBuf), String> {
    // Node / TypeScript
    let pkg_path = root.join("package.json");
    if pkg_path.exists() {
        if !on_path("node") {
            return Err("this server needs Node.js (node/npm on PATH)".into());
        }
        let pkg: Value =
            serde_json::from_str(&std::fs::read_to_string(&pkg_path).map_err(|e| e.to_string())?)
                .map_err(|e| format!("package.json: {e}"))?;
        let pm = if root.join("pnpm-lock.yaml").exists() && on_path("pnpm") {
            "pnpm"
        } else if root.join("yarn.lock").exists() && on_path("yarn") {
            "yarn"
        } else if root.join("bun.lockb").exists() && on_path("bun") {
            "bun"
        } else {
            "npm"
        };
        run(pm, &["install"], root, log).await?;
        let scripts = pkg.get("scripts").and_then(Value::as_object);
        if scripts.is_some_and(|s| s.contains_key("build")) {
            run(pm, &["run", "build"], root, log).await?;
        }
        // entry: bin → main → common build outputs
        let bin_entry = match pkg.get("bin") {
            Some(Value::String(s)) => Some(s.clone()),
            Some(Value::Object(o)) => o.values().next().and_then(Value::as_str).map(str::to_string),
            _ => None,
        };
        let candidates: Vec<String> = bin_entry
            .into_iter()
            .chain(pkg.get("main").and_then(Value::as_str).map(str::to_string))
            .chain(
                [
                    "dist/index.js",
                    "build/index.js",
                    "dist/server.js",
                    "build/server.js",
                    "index.js",
                    "server.js",
                    "dist/cli.js",
                ]
                .iter()
                .map(|s| s.to_string()),
            )
            .collect();
        let entry = candidates
            .iter()
            .find(|c| root.join(c).is_file())
            .ok_or_else(|| {
                format!(
                    "built, but no entry point found (tried {})",
                    candidates.join(", ")
                )
            })?;
        return Ok((
            "node".into(),
            vec!["node".into(), root.join(entry).display().to_string()],
            root.to_path_buf(),
        ));
    }
    // Python
    let pyproject = root.join("pyproject.toml");
    if pyproject.exists() {
        let text = std::fs::read_to_string(&pyproject).unwrap_or_default();
        let script = text
            .split("[project.scripts]")
            .nth(1)
            .and_then(|s| {
                s.lines()
                    .map(str::trim)
                    .find(|l| !l.is_empty() && !l.starts_with('[') && l.contains('='))
            })
            .and_then(|l| l.split('=').next())
            .map(|s| s.trim().trim_matches('"').to_string());
        if on_path("uv") {
            run("uv", &["sync"], root, log).await.ok();
            let cmd = match &script {
                Some(s) => vec![
                    "uv".to_string(),
                    "run".into(),
                    "--directory".into(),
                    root.display().to_string(),
                    s.clone(),
                ],
                None => {
                    let module = root
                        .join("src")
                        .read_dir()
                        .ok()
                        .and_then(|rd| {
                            rd.flatten()
                                .find(|e| {
                                    e.path().is_dir()
                                        && !e.file_name().to_string_lossy().ends_with(".egg-info")
                                })
                                .map(|e| e.file_name().to_string_lossy().to_string())
                        })
                        .unwrap_or_else(|| "server".into());
                    vec![
                        "uv".to_string(),
                        "run".into(),
                        "--directory".into(),
                        root.display().to_string(),
                        "python".into(),
                        "-m".into(),
                        module,
                    ]
                }
            };
            return Ok(("python (uv)".into(), cmd, root.to_path_buf()));
        }
        if on_path("python3") {
            run("python3", &["-m", "venv", ".venv"], root, log).await?;
            let pip = root.join(".venv/bin/pip").display().to_string();
            run(&pip, &["install", "-q", "-e", "."], root, log).await?;
            let cmd = match &script {
                Some(s) => vec![root.join(".venv/bin").join(s).display().to_string()],
                None => vec![
                    root.join(".venv/bin/python").display().to_string(),
                    "-m".into(),
                    "server".into(),
                ],
            };
            return Ok(("python (venv)".into(), cmd, root.to_path_buf()));
        }
        return Err("this server needs Python: install uv (recommended) or python3".into());
    }
    // Rust
    if root.join("Cargo.toml").exists() {
        if !on_path("cargo") {
            return Err("this server needs a Rust toolchain (cargo)".into());
        }
        run("cargo", &["build", "--release", "--quiet"], root, log).await?;
        let bin = std::fs::read_dir(root.join("target/release"))
            .ok()
            .and_then(|rd| {
                rd.flatten().map(|e| e.path()).find(|p| {
                    p.is_file()
                        && p.extension().is_none()
                        && !p.file_name().unwrap().to_string_lossy().starts_with('.')
                })
            })
            .ok_or("built, but no binary found in target/release")?;
        return Ok(("rust".into(), vec![bin.display().to_string()], root.to_path_buf()));
    }
    // Go
    if root.join("go.mod").exists() {
        if !on_path("go") {
            return Err("this server needs Go".into());
        }
        let out = root.join("bin/server");
        run("go", &["build", "-o", &out.display().to_string(), "."], root, log).await?;
        return Ok(("go".into(), vec![out.display().to_string()], root.to_path_buf()));
    }
    Err("no package.json, pyproject.toml, Cargo.toml or go.mod found — point at the server's sub-folder (…/tree/main/src/<server>) or use npm:<package> / pypi:<package>".into())
}

/// Write the `mcp.<name>` entry into a config file (project or global).
pub fn register(config_path: &Path, installed: &McpInstalled) -> Result<(), String> {
    let mut root: serde_json::Map<String, Value> = match std::fs::read_to_string(config_path) {
        Ok(text) => crate::config::parse_jsonc(&text, config_path)
            .map_err(|e| e.to_string())?
            .as_object()
            .cloned()
            .unwrap_or_default(),
        Err(_) => serde_json::Map::new(),
    };
    let mcp = root
        .entry("mcp")
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    if !mcp.is_object() {
        *mcp = Value::Object(serde_json::Map::new());
    }
    let mut entry = serde_json::json!({ "type": "local", "command": installed.command });
    if let Some(cwd) = &installed.cwd {
        entry["cwd"] = Value::String(cwd.clone());
    }
    mcp.as_object_mut().unwrap().insert(installed.name.clone(), entry);
    if let Some(parent) = config_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(
        config_path,
        serde_json::to_string_pretty(&Value::Object(root)).map_err(|e| e.to_string())? + "\n",
    )
    .map_err(|e| e.to_string())
}

/// The config file an `mcp` entry should go to.
pub fn config_file(paths: &Paths, directory: &Path, global: bool) -> PathBuf {
    if global {
        ["lunarzero.jsonc", "lunarzero.json", "config.json"]
            .iter()
            .map(|n| paths.config.join(n))
            .find(|p| p.exists())
            .unwrap_or_else(|| paths.config.join("lunarzero.json"))
    } else {
        [
            "lunarzero.jsonc",
            "lunarzero.json",
            "opencode.jsonc",
            "opencode.json",
        ]
        .iter()
        .map(|n| directory.join(n))
        .find(|p| p.exists())
        .unwrap_or_else(|| directory.join("lunarzero.json"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(
            default_name(
                "https://github.com/modelcontextprotocol/servers.git",
                Some("src/filesystem")
            ),
            "filesystem"
        );
        assert_eq!(
            default_name("@modelcontextprotocol/server-github", None),
            "github"
        );
        assert_eq!(
            default_name("https://github.com/acme/weather-mcp-server.git", None),
            "weather"
        );
        assert_eq!(default_name("mcp-server-git", None), "git");
    }
}
