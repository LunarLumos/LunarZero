//! `bash` tool: run a shell command with timeout, rolling output buffer,
//! process-group kill on abort, and per-sub-command permission patterns
//!.

use std::borrow::Cow;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::AsyncReadExt;

use crate::permission::arity;
use crate::tool::{Tool, ToolCtx, ToolError, ToolResult, parse_args, truncate};

const DEFAULT_TIMEOUT_MS: u64 = 2 * 60 * 1000;
/// Default for package installs, builds, container and VM start-ups when the
/// model gives no timeout — these routinely take longer than two minutes.
const SLOW_TIMEOUT_MS: u64 = 10 * 60 * 1000;
/// How many times an identical line is kept before the rest are collapsed.
const REPEAT_KEEP: usize = 3;

/// Source argument of an `lz skill install` / `lz mcp install` invocation
/// anywhere in the command line, if present.
pub fn install_source(cmd: &str) -> Option<String> {
    let toks: Vec<&str> = cmd
        .split(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | ';' | '&' | '|' | '(' | ')'))
        .filter(|t| !t.is_empty())
        .collect();
    for i in 0..toks.len() {
        let bin = toks[i];
        let is_lz = bin == "lz"
            || bin == "lunarzero"
            || bin.ends_with("/lz")
            || bin.ends_with("/lunarzero")
            || bin.contains("LZ_BIN");
        if is_lz
            && matches!(toks.get(i + 1), Some(&"skill") | Some(&"mcp"))
            && toks.get(i + 2) == Some(&"install")
        {
            return toks[i + 3..]
                .iter()
                .find(|t| !t.starts_with('-'))
                .map(|t| t.to_string());
        }
    }
    None
}

/// Did the user's own latest message ask for this install? The source (or
/// its last path segment / package name) must appear in it.
pub fn user_requested_install(user_text: &str, source: &str) -> bool {
    let text = user_text.to_ascii_lowercase();
    if text.is_empty() {
        return false;
    }
    let src = source.to_ascii_lowercase();
    let mut needles: Vec<String> = vec![src.clone()];
    let bare = src
        .trim_end_matches('/')
        .trim_end_matches(".git")
        .rsplit('/')
        .next()
        .unwrap_or(&src)
        .to_string();
    if bare.len() >= 3 {
        needles.push(bare.clone());
    }
    if let Some((_, pkg)) = src.split_once(':')
        && pkg.len() >= 3
    {
        needles.push(pkg.rsplit('/').next().unwrap_or(pkg).to_string());
    }
    let mentions_install = ["install", "add", "set up", "setup", "connect", "use"]
        .iter()
        .any(|w| text.contains(w));
    mentions_install && needles.iter().any(|n| text.contains(n.as_str()))
}

fn is_slow_command(cmd: &str) -> bool {
    const SLOW: &[&str] = &[
        "npm install",
        "npm ci",
        "npm i ",
        "npm i\n",
        "npm run build",
        "npm test",
        "npx ",
        "pnpm install",
        "pnpm i ",
        "pnpm build",
        "yarn",
        "bun install",
        "pip install",
        "pip3 install",
        "uv sync",
        "uv pip",
        "poetry install",
        "cargo build",
        "cargo test",
        "cargo install",
        "cargo run",
        "cargo check",
        "cargo clippy",
        "go build",
        "go test",
        "go mod",
        "docker ",
        "docker-compose",
        "colima ",
        "podman ",
        "prisma ",
        "next build",
        "vite build",
        "tsc",
        "make",
        "cmake",
        "gradle",
        "gradlew",
        "mvn ",
        "bundle install",
        "composer install",
        "brew install",
        "apt",
        "dnf ",
        "pacman",
        "mix deps",
        "swift build",
        "xcodebuild",
        "flutter ",
        "terraform",
        "pulumi",
        "vercel ",
        "wrangler",
        "playwright",
        "cypress",
        "jest",
        "vitest",
        "pytest",
    ];
    let c = format!("{} ", cmd.trim());
    SLOW.iter().any(|k| c.contains(k))
}

/// Drop lines that already appeared `REPEAT_KEEP` times (identical after
/// trimming, at least 16 chars) so warning spam cannot flood the context;
/// a summary of what was dropped is appended.
fn collapse_repeats(text: &str) -> String {
    use std::collections::HashMap;
    let mut seen: HashMap<&str, usize> = HashMap::new();
    let mut out: Vec<&str> = Vec::new();
    let mut dropped = 0usize;
    for line in text.split('\n') {
        let key = line.trim();
        if key.len() < 16 {
            out.push(line);
            continue;
        }
        let n = seen.entry(key).or_insert(0);
        *n += 1;
        if *n > REPEAT_KEEP {
            dropped += 1;
        } else {
            out.push(line);
        }
    }
    if dropped == 0 {
        return text.to_string();
    }
    let distinct = seen.values().filter(|&&n| n > REPEAT_KEEP).count();
    format!(
        "{}\n[{dropped} repeated lines collapsed ({distinct} distinct); full output saved]",
        out.join("\n")
    )
}
const MAX_METADATA_LENGTH: usize = 30_000;
const CWD_CMDS: &[&str] = &["cd", "chdir", "popd", "pushd"];
const FILE_CMDS: &[&str] = &[
    "cd", "chdir", "popd", "pushd", "rm", "cp", "mv", "mkdir", "touch", "chmod", "chown", "cat",
];

#[derive(Deserialize)]
struct Args {
    command: String,
    #[serde(default)]
    timeout: Option<i64>,
    #[serde(default)]
    workdir: Option<String>,
}

pub struct BashTool;

/// Pick the shell: config `shell` → `$SHELL` → `/bin/sh`; fish/nu are refused
/// because their syntax breaks the `-c` contract.
pub fn select_shell(configured: Option<&str>) -> String {
    let candidate = configured
        .map(str::to_string)
        .or_else(|| std::env::var("SHELL").ok())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "/bin/sh".into());
    let name = Path::new(&candidate)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    if matches!(name, "fish" | "nu" | "nushell") {
        for fallback in ["/bin/zsh", "/bin/bash", "/bin/sh"] {
            if Path::new(fallback).exists() {
                return fallback.into();
            }
        }
    }
    candidate
}

fn shell_name(shell: &str) -> String {
    Path::new(shell)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("sh")
        .to_string()
}

/// Split a command line into sub-commands on `&&`, `||`, `;`, `|`, and newlines,
/// respecting quotes. Each entry is (source text, tokens).
pub fn split_commands(command: &str) -> Vec<(String, Vec<String>)> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut chars = command.chars().peekable();
    let mut depth = 0i32;
    let flush = |cur: &mut String, out: &mut Vec<(String, Vec<String>)>| {
        let src = cur.trim().to_string();
        if !src.is_empty() {
            let tokens = shell_words::split(&src)
                .unwrap_or_else(|_| src.split_whitespace().map(str::to_string).collect());
            out.push((src, tokens));
        }
        cur.clear();
    };
    while let Some(c) = chars.next() {
        match quote {
            Some(q) => {
                cur.push(c);
                if c == '\\' && q == '"' {
                    if let Some(n) = chars.next() {
                        cur.push(n);
                    }
                } else if c == q {
                    quote = None;
                }
            }
            None => match c {
                '\'' | '"' => {
                    quote = Some(c);
                    cur.push(c);
                }
                '\\' => {
                    cur.push(c);
                    if let Some(n) = chars.next() {
                        cur.push(n);
                    }
                }
                '(' | '{' => {
                    depth += 1;
                    cur.push(c);
                }
                ')' | '}' => {
                    depth -= 1;
                    cur.push(c);
                }
                '&' if depth <= 0 && chars.peek() == Some(&'&') => {
                    chars.next();
                    flush(&mut cur, &mut out);
                }
                '|' if depth <= 0 => {
                    if chars.peek() == Some(&'|') {
                        chars.next();
                    }
                    flush(&mut cur, &mut out);
                }
                ';' | '\n' if depth <= 0 => flush(&mut cur, &mut out),
                _ => cur.push(c),
            },
        }
    }
    flush(&mut cur, &mut out);
    // a `$(...)` / backtick body may hide more commands; scan them too
    let mut nested = Vec::new();
    for (src, _) in &out {
        let mut rest = src.as_str();
        while let Some(i) = rest.find("$(") {
            let after = &rest[i + 2..];
            let Some(j) = after.find(')') else { break };
            nested.extend(split_commands(&after[..j]));
            rest = &after[j + 1..];
        }
    }
    out.extend(nested);
    out
}

fn unquote(s: &str) -> &str {
    let b = s.as_bytes();
    if b.len() >= 2 && (b[0] == b'"' || b[0] == b'\'') && b[0] == b[b.len() - 1] {
        &s[1..s.len() - 1]
    } else {
        s
    }
}

fn is_dynamic(s: &str) -> bool {
    s.starts_with('(') || s.contains("$(") || s.contains("${") || s.contains('`') || s.contains('$')
}

fn glob_prefix(s: &str) -> Option<&str> {
    match s.find(['?', '*', '[']) {
        None => Some(s),
        Some(0) => None,
        Some(i) => Some(&s[..i]),
    }
}

pub struct Scan {
    pub dirs: BTreeSet<PathBuf>,
    pub patterns: BTreeSet<String>,
    pub always: BTreeSet<String>,
}

pub fn scan(command: &str, cwd: &Path, worktree: &Path, directory: &Path, home: &Path) -> Scan {
    let mut s = Scan {
        dirs: BTreeSet::new(),
        patterns: BTreeSet::new(),
        always: BTreeSet::new(),
    };
    for (src, tokens) in split_commands(command) {
        let Some(cmd) = tokens.first() else { continue };
        if FILE_CMDS.contains(&cmd.as_str()) {
            for arg in tokens.iter().skip(1) {
                if arg.starts_with('-') || (cmd == "chmod" && arg.starts_with('+')) {
                    continue;
                }
                let text = crate::paths::expand_home(unquote(arg), home);
                let text_s = text.to_string_lossy();
                let Some(prefix) = glob_prefix(&text_s) else {
                    continue;
                };
                if prefix.is_empty() || is_dynamic(prefix) {
                    continue;
                }
                let resolved = if Path::new(prefix).is_absolute() {
                    PathBuf::from(prefix)
                } else {
                    cwd.join(prefix)
                };
                if resolved.starts_with(worktree) || resolved.starts_with(directory) {
                    continue;
                }
                let dir = if resolved.is_dir() {
                    resolved
                } else {
                    resolved.parent().map(Path::to_path_buf).unwrap_or(resolved)
                };
                s.dirs.insert(dir);
            }
        }
        if !CWD_CMDS.contains(&cmd.as_str()) {
            s.patterns.insert(src.clone());
            s.always.insert(format!("{} *", arity::prefix(&tokens).join(" ")));
        }
    }
    s
}

fn preview(text: &str) -> String {
    if text.len() <= MAX_METADATA_LENGTH {
        return text.to_string();
    }
    let start = text.len() - MAX_METADATA_LENGTH;
    let start = (start..text.len())
        .find(|i| text.is_char_boundary(*i))
        .unwrap_or(text.len());
    format!("...\n\n{}", &text[start..])
}

fn description(shell: &str, max_lines: usize, max_bytes: usize) -> String {
    let template = crate::tool_description!("bash");
    let name = shell_name(shell);
    let chain = "If the commands depend on each other and must run sequentially, use a single Bash call with '&&' to chain them together (e.g., `git add . && git commit -m \"message\" && git push`). For instance, if one operation must complete before another starts (like mkdir before cp, Write before Bash for git operations, or git add before git commit), run these operations sequentially instead.";
    let command_section = format!(
        r#"Before executing the command, please follow these steps:

1. Directory Verification:
   - If the command will create new directories or files, first use `ls` to verify the parent directory exists and is the correct location
   - For example, before running "mkdir foo/bar", first use `ls foo` to check that "foo" exists and is the intended parent directory

2. Command Execution:
   - Always quote file paths that contain spaces with double quotes (e.g., rm "path with spaces/file.txt")
   - After ensuring proper quoting, execute the command.
   - Capture the output of the command.

Usage notes:
  - The command argument is required.
  - You can specify an optional timeout in milliseconds. If not specified, commands will time out after {DEFAULT_TIMEOUT_MS}ms; installs, builds, docker and similar slow commands get {SLOW_TIMEOUT_MS}ms automatically. Never wrap commands in GNU `timeout` (absent on macOS) — use this parameter. Start servers with `&` or in a separate step so the call returns.
  - Lines that repeat more than a few times (warning spam) are collapsed; the full output is still saved to a file.
  - If the output exceeds {max_lines} lines or {max_bytes} bytes, it will be truncated and the full output will be written to a file. You can use Read with offset/limit to read specific sections or Grep to search the full content. Do NOT use `head`, `tail`, or other truncation commands to limit output; the full output will already be captured to a file for more precise searching.

  - Avoid using Bash with the `find`, `grep`, `cat`, `head`, `tail`, `sed`, `awk`, or `echo` commands, unless explicitly instructed or when these commands are truly necessary for the task. Instead, always prefer using the dedicated tools for these commands:
    - File search: Use Glob (NOT find or ls)
    - Content search: Use Grep (NOT grep or rg)
    - Read files: Use Read (NOT cat/head/tail)
    - Edit files: Use Edit (NOT sed/awk)
    - Write files: Use Write (NOT echo >/cat <<EOF)
    - Communication: Output text directly (NOT echo/printf)
  - When issuing multiple commands:
    - If the commands are independent and can run in parallel, make multiple bash tool calls in a single message. For example, if you need to run "git status" and "git diff", send a single message with two bash tool calls in parallel.
    - {chain}
    - Use ';' only when you need to run commands sequentially but don't care if earlier commands fail
    - DO NOT use newlines to separate commands (newlines are ok in quoted strings)
  - AVOID using `cd <directory> && <command>`. Use the `workdir` parameter to change directories instead.
    <good-example>
    Use workdir="/foo/bar" with command: pytest tests
    </good-example>
    <bad-example>
    cd /foo/bar && pytest tests
    </bad-example>"#
    );
    template
        .replace("${intro}", "Executes a given bash command in a persistent shell session with optional timeout, ensuring proper handling and security measures.")
        .replace("${os}", std::env::consts::OS)
        .replace("${shell}", &name)
        .replace("${tmp}", &std::env::temp_dir().join("lunarzero").display().to_string())
        .replace("${workdirSection}", "All commands run in the current working directory by default. Use the `workdir` parameter if you need to run a command in a different directory. AVOID using `cd <directory> && <command>` patterns - use `workdir` instead.")
        .replace("${commandSection}", &command_section)
}

#[async_trait]
impl Tool for BashTool {
    fn id(&self) -> &'static str {
        "bash"
    }
    fn description(&self) -> Cow<'static, str> {
        Cow::Owned(description(
            &select_shell(None),
            truncate::MAX_LINES,
            truncate::MAX_BYTES,
        ))
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "Shell command" },
                "timeout": { "type": "integer", "description": "Timeout ms" },
                "workdir": { "type": "string", "description": "Working directory (instead of cd)" }
            },
            "required": ["command"]
        })
    }

    async fn execute(&self, ctx: ToolCtx, args: Value) -> Result<ToolResult, ToolError> {
        let args: Args = parse_args(args)?;
        let config = ctx.engine.config();
        let shell = select_shell(config.shell.as_deref());
        let cwd = match &args.workdir {
            Some(w) => ctx.engine.resolve_path(w),
            None => ctx.directory().to_path_buf(),
        };
        if let Some(t) = args.timeout
            && t < 0
        {
            return Err(ToolError::Invalid(format!(
                "Invalid timeout value: {t}. Timeout must be a positive number."
            )));
        }
        let timeout_ms = args.timeout.map(|t| t as u64).unwrap_or_else(|| {
            if is_slow_command(&args.command) {
                SLOW_TIMEOUT_MS
            } else {
                DEFAULT_TIMEOUT_MS
            }
        });

        // permissions
        let mut scanned = scan(
            &args.command,
            &cwd,
            ctx.worktree(),
            ctx.directory(),
            &ctx.engine.paths.home,
        );
        if !cwd.starts_with(ctx.worktree()) && !cwd.starts_with(ctx.directory()) {
            scanned.dirs.insert(cwd.clone());
        }
        if !scanned.dirs.is_empty() {
            let globs: Vec<String> = scanned
                .dirs
                .iter()
                .map(|d| format!("{}/*", d.display()))
                .collect();
            ctx.ask(
                "external_directory",
                globs.clone(),
                globs.clone(),
                json!({ "command": args.command, "directories": scanned.dirs.iter().map(|d| d.display().to_string()).collect::<Vec<_>>(), "patterns": globs })
                    .as_object()
                    .cloned()
                    .unwrap_or_default(),
            )
            .await?;
        }
        // The built-in installers clone, build and run third-party code. When
        // the user asked for exactly this install it is an ordinary command;
        // when the model picked it up elsewhere (a README, a web page, a
        // tool result) a human must confirm, whatever the permission mode.
        if let Some(source) = install_source(&args.command)
            && !user_requested_install(&ctx.last_user_text(), &source)
        {
            ctx.ask_forced(
                "install",
                vec![source.clone()],
                json!({
                    "command": args.command,
                    "source": source,
                    "reason": "The agent wants to install third-party code you did not ask for. It will be cloned, built and run on this machine."
                })
                .as_object()
                .cloned()
                .unwrap_or_default(),
            )
            .await?;
        }
        if !scanned.patterns.is_empty() {
            ctx.ask(
                "bash",
                scanned.patterns.iter().cloned().collect(),
                scanned.always.iter().cloned().collect(),
                json!({ "command": args.command })
                    .as_object()
                    .cloned()
                    .unwrap_or_default(),
            )
            .await?;
        }

        let max_lines = config.tool_output_max_lines();
        let max_bytes = config.tool_output_max_bytes();
        let keep = max_bytes * 2;
        ctx.report(None, Some(json!({ "output": "" })));

        let mut cmd = tokio::process::Command::new(&shell);
        if let Ok(exe) = std::env::current_exe()
            && let Some(bin) = exe.parent()
        {
            let path = std::env::var("PATH").unwrap_or_default();
            if !path.split(':').any(|p| Path::new(p) == bin) {
                cmd.env("PATH", format!("{}:{path}", bin.display()));
            }
            // exact path to this binary, immune to shell rc files reordering PATH
            cmd.env("LZ_BIN", &exe);
        }
        cmd.arg("-c")
            .arg(&args.command)
            .current_dir(&cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(unix)]
        {
            cmd.process_group(0);
        }
        let mut child = cmd
            .spawn()
            .map_err(|e| ToolError::Other(format!("failed to spawn {shell}: {e}")))?;
        let pid = child.id();
        let mut stdout = child.stdout.take();
        let mut stderr = child.stderr.take();

        // rolling buffer of chunks (last `keep` bytes) + full output spilled to file when large
        let mut chunks: std::collections::VecDeque<String> = std::collections::VecDeque::new();
        let mut used = 0usize;
        let mut full = String::new();
        let mut spill: Option<(PathBuf, std::fs::File)> = None;
        let mut last = String::new();
        let mut cut = false;
        let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(64);
        let tx2 = tx.clone();
        let out_task = tokio::spawn(async move {
            if let Some(mut s) = stdout.take() {
                let mut buf = vec![0u8; 8192];
                loop {
                    match s.read(&mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            if tx
                                .send(String::from_utf8_lossy(&buf[..n]).to_string())
                                .await
                                .is_err()
                            {
                                break;
                            }
                        }
                    }
                }
            }
        });
        let err_task = tokio::spawn(async move {
            if let Some(mut s) = stderr.take() {
                let mut buf = vec![0u8; 8192];
                loop {
                    match s.read(&mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            if tx2
                                .send(String::from_utf8_lossy(&buf[..n]).to_string())
                                .await
                                .is_err()
                            {
                                break;
                            }
                        }
                    }
                }
            }
        });

        let deadline = tokio::time::sleep(Duration::from_millis(timeout_ms + 100));
        tokio::pin!(deadline);
        let mut expired = false;
        let mut aborted = false;
        let mut exit_code: Option<i32> = None;
        let mut readers_done = false;
        let mut wait_done = false;
        loop {
            tokio::select! {
                biased;
                _ = ctx.cancel.cancelled(), if !aborted && !expired => {
                    aborted = true;
                    kill_group(pid, &mut child).await;
                }
                _ = &mut deadline, if !expired && !aborted => {
                    expired = true;
                    kill_group(pid, &mut child).await;
                }
                chunk = rx.recv(), if !readers_done => match chunk {
                    None => readers_done = true,
                    Some(chunk) => {
                        let size = chunk.len();
                        used += size;
                        chunks.push_back(chunk.clone());
                        while used > keep && chunks.len() > 1 {
                            if let Some(old) = chunks.pop_front() {
                                used -= old.len();
                                cut = true;
                            }
                        }
                        last = preview(&format!("{last}{chunk}"));
                        match &mut spill {
                            Some((_, f)) => {
                                use std::io::Write;
                                let _ = f.write_all(chunk.as_bytes());
                            }
                            None => {
                                full.push_str(&chunk);
                                if full.len() > max_bytes
                                    && let Ok(path) = truncate::write(&ctx.engine.paths.tool_output(), &full)
                                        && let Ok(f) = std::fs::OpenOptions::new().append(true).open(&path) {
                                            spill = Some((path, f));
                                            cut = true;
                                            full.clear();
                                        }
                            }
                        }
                        ctx.report(None, Some(json!({ "output": last })));
                    }
                },
                status = child.wait(), if !wait_done => {
                    wait_done = true;
                    exit_code = status.ok().and_then(|s| s.code());
                }
            }
            if wait_done && readers_done {
                break;
            }
            if wait_done && (aborted || expired) {
                // don't hang on readers held open by grandchildren
                let _ = tokio::time::timeout(Duration::from_millis(500), async {
                    while let Some(c) = rx.recv().await {
                        chunks.push_back(c);
                    }
                })
                .await;
                break;
            }
        }
        out_task.abort();
        err_task.abort();
        drop(spill);

        let mut meta = Vec::new();
        if expired {
            meta.push(format!("shell tool terminated command after exceeding timeout {timeout_ms} ms. If this command is expected to take longer and is not waiting for interactive input, retry with a larger timeout value in milliseconds."));
        }
        if aborted {
            meta.push("User aborted the command".to_string());
        }
        let raw: String = chunks.iter().map(String::as_str).collect();
        let full_path = if raw.len() > 4096 && collapse_repeats(&raw).len() < raw.len() {
            truncate::write(&ctx.engine.paths.tool_output(), &raw).ok()
        } else {
            None
        };
        let raw = match &full_path {
            Some(p) => format!("{}\nFull output: {}", collapse_repeats(&raw), p.display()),
            None => raw,
        };
        let tail = truncate::output(
            &ctx.engine.paths.tool_output(),
            &raw,
            max_lines,
            max_bytes,
            truncate::Direction::Tail,
            false,
        );
        let mut output = if tail.truncated {
            cut = true;
            // truncate::output already appended a saved-file hint
            tail.content
        } else {
            raw.clone()
        };
        if output.is_empty() {
            output = "(no output)".into();
        }
        if !meta.is_empty() {
            output.push_str(&format!(
                "\n\n<shell_metadata>\n{}\n</shell_metadata>",
                meta.join("\n")
            ));
        }
        let output_path = tail.output_path.map(|p| p.display().to_string());
        Ok(ToolResult {
            title: args.command.clone(),
            metadata: json!({ "output": if last.is_empty() { preview(&output) } else { last }, "exit": exit_code, "truncated": cut, "outputPath": output_path }),
            output,
            attachments: Vec::new(),
        })
    }
}

#[cfg_attr(not(unix), allow(unused_variables))]
async fn kill_group(pid: Option<u32>, child: &mut tokio::process::Child) {
    #[cfg(unix)]
    {
        if let Some(pid) = pid {
            unsafe {
                libc::killpg(pid as i32, libc::SIGTERM);
            }
            let _ = tokio::time::timeout(Duration::from_secs(3), child.wait()).await;
            unsafe {
                libc::killpg(pid as i32, libc::SIGKILL);
            }
            return;
        }
    }
    let _ = child.kill().await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_and_scans() {
        let cmds = split_commands("git add . && rm -rf /tmp/x | sort; echo \"a && b\"");
        let srcs: Vec<&str> = cmds.iter().map(|(s, _)| s.as_str()).collect();
        assert_eq!(
            srcs,
            vec!["git add .", "rm -rf /tmp/x", "sort", "echo \"a && b\""]
        );
        let s = scan(
            "cd /somewhere && git status",
            Path::new("/proj"),
            Path::new("/proj"),
            Path::new("/proj"),
            Path::new("/home"),
        );
        assert!(s.patterns.contains("git status"));
        assert!(!s.patterns.iter().any(|p| p.starts_with("cd ")));
        assert!(s.always.contains("git status *"));
        assert!(s.dirs.contains(Path::new("/somewhere")) || s.dirs.contains(Path::new("/")));
    }

    #[test]
    fn nested_subshell() {
        let cmds = split_commands("echo $(rm foo)");
        assert!(cmds.iter().any(|(s, _)| s == "rm foo"));
    }
}

#[cfg(test)]
mod output_tests {
    use super::*;

    #[test]
    fn slow_commands_get_long_timeout() {
        assert!(is_slow_command("cd app && npm install"));
        assert!(is_slow_command("docker-compose up -d"));
        assert!(is_slow_command("colima start"));
        assert!(is_slow_command("npx prisma migrate deploy"));
        assert!(!is_slow_command("ls -la"));
        assert!(!is_slow_command("git status"));
    }

    #[test]
    fn install_gate() {
        assert_eq!(
            install_source("\"$LZ_BIN\" mcp install https://github.com/org/server --name x"),
            Some("https://github.com/org/server".into())
        );
        assert_eq!(
            install_source("lz skill install pdf --project"),
            Some("pdf".into())
        );
        assert_eq!(
            install_source("cd app && /usr/local/bin/lz mcp install npm:@scope/srv"),
            Some("npm:@scope/srv".into())
        );
        assert_eq!(install_source("lz mcp list"), None);
        assert_eq!(install_source("npm install react"), None);
        assert!(user_requested_install(
            "please install the github mcp server",
            "github"
        ));
        assert!(user_requested_install(
            "add https://github.com/org/server as an mcp",
            "https://github.com/org/server"
        ));
        assert!(user_requested_install("install npm:@scope/srv", "npm:@scope/srv"));
        assert!(!user_requested_install(
            "fix the failing tests",
            "https://github.com/org/server"
        ));
        assert!(!user_requested_install("", "pdf"));
        assert!(!user_requested_install(
            "install the pdf skill",
            "https://github.com/evil/other"
        ));
    }

    #[test]
    fn repeated_lines_collapse() {
        let block =
            "npm warn ERESOLVE overriding peer dependency\nnpm warn While resolving: @effect/sql-d1@4.0.0\n";
        let text = format!("{}done: ok\n", block.repeat(50));
        let out = collapse_repeats(&text);
        assert!(out.matches("ERESOLVE").count() == 3, "{out}");
        assert!(out.contains("done: ok"));
        assert!(out.contains("94 repeated lines collapsed (2 distinct)"), "{out}");
        assert_eq!(collapse_repeats("a\nb\nc"), "a\nb\nc");
    }
}
