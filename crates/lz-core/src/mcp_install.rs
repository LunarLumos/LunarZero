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
    // curated alias, e.g. `github` → npm:@modelcontextprotocol/server-github (+ default args)
    if let Some(rec) = crate::recommended::mcp(src) {
        let mut r = Box::pin(install(
            paths,
            &rec.source,
            Some(name.unwrap_or_else(|| rec.alias.clone())),
        ))
        .await?;
        r.command.extend(rec.args.iter().cloned());
        if !rec.env.is_empty() {
            r.notes.push(format!("needs env: {}", rec.env.join(", ")));
        }
        if !rec.note.is_empty() {
            r.notes.push(rec.note.clone());
        }
        return Ok(r);
    }
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
    // no manifest at the root: the README usually says how to run it
    if parsed.subpath.is_none() && !has_manifest(&root) {
        if let Some(pkg) = readme_package(&root) {
            log.push(format!(
                "README suggests `{pkg}`; using the package instead of building the repository"
            ));
            let _ = std::fs::remove_dir_all(&dir);
            let mut r = Box::pin(install(paths, &pkg, Some(name))).await?;
            r.notes.append(&mut log);
            return Ok(r);
        }
        // exactly one sub-project → use it; several → ask which
        let subs = sub_projects(&root);
        if subs.len() == 1 {
            log.push(format!("using sub-folder {}", subs[0]));
            let r = root.join(&subs[0]);
            let (runtime, command, cwd) = detect_and_build(paths, &r, &mut log).await?;
            let _ = std::fs::write(dir.join(".lz-mcp.json"), serde_json::json!({ "source": parsed.url, "git_ref": parsed.git_ref, "subpath": subs[0], "installed_at": now_ms() }).to_string());
            return Ok(McpInstalled {
                name,
                command,
                cwd: Some(cwd.display().to_string()),
                runtime,
                dir: Some(dir.display().to_string()),
                notes: log,
            });
        }
    }
    // a repository that bundles several servers: ask for one instead of
    // building a launcher that needs arguments
    if parsed.subpath.is_none()
        && let Some((sub, servers)) = bundled_servers(&root).or_else(|| {
            let subs = sub_projects(&root);
            (subs.len() >= 2).then(|| (".".to_string(), subs))
        })
    {
        let _ = std::fs::remove_dir_all(&dir);
        let base = src.trim_end_matches('/').trim_end_matches(".git");
        let base = if base.contains("://") {
            base.to_string()
        } else {
            format!("https://github.com/{base}")
        };
        let branch = parsed.git_ref.clone().unwrap_or_else(|| "main".into());
        return Err(format!(
            "{} bundles {} servers{}: {}.\nInstall one at a time, e.g.: lz mcp install {base}/tree/{branch}/{}{}",
            parsed.url,
            servers.len(),
            if sub == "." {
                String::new()
            } else {
                format!(" under {sub}/")
            },
            servers.join(", "),
            if sub == "." {
                String::new()
            } else {
                format!("{sub}/")
            },
            servers[0]
        ));
    }
    let (runtime, command, cwd) = detect_and_build(paths, &root, &mut log).await?;
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

/// `uv` from PATH, or a private copy installed once with pip into
/// `~/.local/share/lunarzero/tools/` (uv ships as a wheel with a static binary).
pub async fn ensure_uv(paths: &Paths, log: &mut Vec<String>) -> Result<String, String> {
    if on_path("uv") {
        return Ok("uv".into());
    }
    let tools = paths.data.join("tools");
    let private = tools.join("venv/bin/uv");
    if private.is_file() {
        return Ok(private.display().to_string());
    }
    let python = ["python3", "python3.13", "python3.12", "python3.11"]
        .into_iter()
        .find(|p| on_path(p))
        .ok_or(
            "Python servers need `uv`; install it with `brew install uv` (no python3 found to bootstrap it)",
        )?;
    std::fs::create_dir_all(&tools).map_err(|e| e.to_string())?;
    log.push("bootstrapping a private uv (one-time)".into());
    run(python, &["-m", "venv", "venv"], &tools, log).await?;
    let pip = tools.join("venv/bin/pip").display().to_string();
    run(
        &pip,
        &["install", "-q", "--disable-pip-version-check", "uv"],
        &tools,
        log,
    )
    .await?;
    if private.is_file() {
        Ok(private.display().to_string())
    } else {
        Err("could not install uv; run `brew install uv` and retry".into())
    }
}

/// `npx @scope/pkg` / `uvx pkg` from the README → `npm:`/`pypi:` source.
fn readme_package(root: &Path) -> Option<String> {
    let text = ["README.md", "readme.md", "README.MD", "README"]
        .iter()
        .find_map(|n| std::fs::read_to_string(root.join(n)).ok())?;
    for line in text.lines() {
        for (prefix, scheme) in [
            ("npx -y ", "npm:"),
            ("npx ", "npm:"),
            ("uvx ", "pypi:"),
            ("pipx run ", "pypi:"),
        ] {
            let Some(i) = line.find(prefix) else { continue };
            // must be at a word boundary (not e.g. "…/npx ")
            if i > 0 && line.as_bytes()[i - 1].is_ascii_alphanumeric() {
                continue;
            }
            {
                let rest = &line[i + prefix.len()..];
                let pkg = rest
                    .split(|c: char| c.is_whitespace() || c == '`' || c == '"' || c == '\'' || c == ')')
                    .next()
                    .unwrap_or("");
                let ok = !pkg.is_empty()
                    && !pkg.starts_with('-')
                    && !pkg.starts_with('<')
                    && (pkg.starts_with('@') || pkg.contains("mcp") || pkg.contains("server"));
                if ok {
                    return Some(format!("{scheme}{pkg}"));
                }
            }
        }
    }
    None
}

/// Relative paths (depth ≤ 2) of directories with a manifest, most likely servers first.
fn sub_projects(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let walker = ignore::WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        .max_depth(Some(2))
        .build();
    for e in walker.flatten() {
        let p = e.path();
        if p == root || !p.is_dir() || !has_manifest(p) {
            continue;
        }
        if p.components().any(|c| {
            matches!(
                c.as_os_str().to_str(),
                Some("node_modules")
                    | Some("shared")
                    | Some("common")
                    | Some("docs")
                    | Some("examples")
                    | Some("tests")
            )
        }) {
            continue;
        }
        if let Ok(rel) = p.strip_prefix(root) {
            out.push(rel.display().to_string());
        }
    }
    out.sort_by_key(|p| (!p.contains("server") && !p.contains("mcp"), p.clone()));
    out
}

fn has_manifest(p: &Path) -> bool {
    ["package.json", "pyproject.toml", "Cargo.toml", "go.mod"]
        .iter()
        .any(|m| p.join(m).is_file())
}

/// `(dir name, server names)` when `root/<dir>/` holds two or more sub-projects.
fn bundled_servers(root: &Path) -> Option<(String, Vec<String>)> {
    for dir in ["servers", "packages", "src", "mcp", "apps", "examples"] {
        let d = root.join(dir);
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        let mut names: Vec<String> = rd
            .flatten()
            .filter(|e| e.path().is_dir() && has_manifest(&e.path()))
            .filter_map(|e| e.file_name().to_str().map(str::to_string))
            .collect();
        names.sort();
        if names.len() >= 2 {
            return Some((dir.to_string(), names));
        }
    }
    None
}

/// Look at the project files, build, and return (runtime, command, cwd).
async fn detect_and_build(
    paths: &Paths,
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
    // Python — always through uv (bootstrapped privately when missing): it
    // honours uv.lock, picks a Python that satisfies requires-python, and
    // keeps every server in its own environment
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
        let uv = ensure_uv(paths, log).await?;
        let mut sync_args = vec!["sync".to_string()];
        if !root.join("uv.lock").exists() {
            sync_args.push("--no-dev".into());
        }
        let sync_ref: Vec<&str> = sync_args.iter().map(String::as_str).collect();
        run(&uv, &sync_ref, root, log).await?;
        let cmd = match &script {
            Some(s) => vec![
                uv.clone(),
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
                                e.path().is_dir() && !e.file_name().to_string_lossy().ends_with(".egg-info")
                            })
                            .map(|e| e.file_name().to_string_lossy().to_string())
                    })
                    .unwrap_or_else(|| "server".into());
                vec![
                    uv.clone(),
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
