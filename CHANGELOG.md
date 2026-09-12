# Changelog

All notable changes, newest first. Dates are commit dates on `main`.

## Unreleased (0.1.0 line)

### 2026-09-12 — trust, measurement, autonomy

**Safety and CI**
- GitHub Actions: fmt, clippy `-D warnings`, tests on Linux and macOS with a release smoke run, Windows build as an experimental job, `cargo audit` on every change and weekly.
- Installing third-party code (`lz skill|mcp install`) that the model picked up from content rather than from the user now needs an explicit confirmation regardless of permission mode or `--auto`; unattended runs refuse it.
- Installer builds run with a scrubbed environment inside `sandbox-exec` (macOS) / `bwrap` (Linux), hiding `~/.ssh`, `~/.aws`, `~/.netrc`, `~/.npmrc`, gh/gcloud config, keychains and `auth.json`; `npm --ignore-scripts` when no OS sandbox exists.
- OS keychain for API keys: `lz auth login --keychain`, `lz auth migrate`, `"auth": {"keychain": true}`.
- Permission engine and provider-failover tests (wildcards, arity, rule ordering, mode layering, forced asks; retry-after, daily quotas, provider-wide auth bench, backoff caps, exhaustion).
- `SECURITY.md`.

**Routing you can inspect**
- Scored, word-boundary task classifier: 99.0% on the 200-prompt labelled set (`assets/eval/routing.jsonl`, `lz pool eval`); CI fails under 90%.
- `pool.policy`: `prefer`, `avoid`, `optimize` (balanced/quality/speed/latency), `local_first`.
- `lz pool why <model>`: limits used, cooldown reason and wait, last error, measured latency, policy effect. `lz pool report`: last 24 h of usage with the list-price equivalent.
- `scripts/bench.py` + `scripts/mock_provider.py`; numbers in `BENCHMARKS.md`.
- Fixed: a non-git directory resolved its worktree to `/` (the index walked the filesystem, every step waited on it, the external-directory guard could never fire). `lz run` no longer blocks on an idle inherited stdin pipe.

**Agent capabilities**
- Background subagents: `task` with `background: true`; results delivered as messages; `task_id` collects; parent abort cascades.
- `symbol` falls back to the language server (`workspace/symbol`, `references`) for what the tree-sitter index doesn't cover.
- Index and skeletons for Java, C, C++, Ruby (plus Rust, Python, JS/TS, Go).
- Repair rounds carry parsed failures — test name, `file:line`, assertion — for cargo/rustc, pytest, jest/vitest, go test, tsc/eslint.
- Live LSP diagnostics after every edit (server started on demand, readiness tracked via `$/progress`, pushes matched to the document version); a turn ending with errors in edited files is repaired before any build runs.
- Post-edit formatting with the project's own formatter (rustfmt, prettier/biome, gofmt, ruff/black, zig, mix, dart); files that were not formatter-clean before the edit are left alone.
- Tree-sitter skeletons: `read … skeleton: true`, and the most relevant file's outline in `<symbols>`.
- Hunk-by-hunk review of edits in the permission panel (`space`/`n`/`p`), partial application with a note to the model.
- Messages sent while a turn runs are queued and delivered at the next step.
- Self-healing loop for failed check commands; plans the model cannot abandon halfway.
- Claude via the native Messages API (thinking blocks, tool use, prompt caching); OpenAI alongside; permission modes manual / accept edits / auto / plan (`shift+tab`).

### 2026-09-11 — the free pool and the first-run experience
- Free-tier pool: 278 models across 13 providers, `lunar/auto` with per-request classification, instant failover, cooldowns with exact `retry-after`, wait-for-soonest, learned latency, sticky sessions, rescue of non-pool models.
- Web portal on the TUI's engine: chat, keys, pool, settings; same-origin loopback auth.
- Skills and MCP servers installed from GitHub / npm / PyPI links, by the CLI, the TUI, the portal or the agent; curated catalog; ten built-in skills.
- Prompt-aware selection of model, skills and MCP servers; project map; token diet (compact prompts and schemas, no reasoning replay, early pruning, old write inputs stubbed).
- Resume interrupted turns; loop guard; hot-reload of config, keys and MCP servers; onboarding, `lz setup`, local server probing.
- `hacker` theme; notifications kept out of the transcript.

### Initial
- Single-binary Rust terminal coding agent: engine, tools, permissions, sessions, compaction, snapshots, MCP, LSP, TUI.
