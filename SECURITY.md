# Security

LunarZero runs shell commands, edits files and installs third-party code on your machine
on behalf of a language model. This page says what it does to keep that safe, and how to
report a problem.

## Reporting a vulnerability

Email **security@lunarzero.dev** or open a
[private security advisory](https://github.com/LunarLumos/LunarZero/security/advisories/new)
on GitHub. Please do not file public issues for exploitable bugs. You will get an
acknowledgement within 72 hours; fixes ship as a patch release and are noted in
[CHANGELOG.md](CHANGELOG.md) with credit unless you prefer otherwise.

In scope: anything that lets a model, a file it reads, a web page it fetches, or an MCP
server it talks to do something the user did not approve — command execution outside the
permission rules, credential exposure, sandbox escapes in the installers, path traversal
past the worktree, prompt-injection paths that bypass a confirmation.

## What the tool does

**Permissions.** Every command and edit is evaluated against an ordered ruleset
(`allow` / `ask` / `deny`, last match wins). The default mode asks before every edit and
command; `accept edits`, `auto` and `plan` modes are explicit choices shown in the footer.
A user's own `permission` config always wins over the mode.

**Installing third-party code.** `lz skill install` and `lz mcp install` clone, build and
run code. When the user typed the request, it is an ordinary command. When the model
picked the install up from anywhere else (a README, a web page, a tool result), the
request is put in front of the user **regardless of permission mode or `--auto`**, and
refused outright when nobody is there to answer.

**Build isolation.** Install builds run with a scrubbed environment (no `*_KEY`, `*TOKEN*`,
`*SECRET*`, cloud/GitHub variables; a private `HOME`; real package caches kept) inside an
OS sandbox where one exists — `sandbox-exec` on macOS, `bwrap` on Linux — that denies
reads of `~/.ssh`, `~/.aws`, `~/.netrc`, `~/.npmrc`, `~/.config/gh`, keychains and
LunarZero's own `auth.json`. Without an OS sandbox, package-manager lifecycle scripts are
skipped (`npm install --ignore-scripts`).

**Credentials.** API keys live in `auth.json` (mode 0600) or, with `--keychain` /
`"auth": {"keychain": true}`, in the OS keychain (macOS Keychain, Secret Service,
Windows Credential Manager) with only a reference on disk. Keys are never written to
logs, session transcripts or the model's context; requests go only to the provider the
key belongs to.

**Network.** The web portal binds to `127.0.0.1` only; cross-origin requests need a
per-run token. The tool makes no calls to any LunarZero service — there is none.

**Supply chain.** CI runs `cargo audit` on every change and weekly; dependencies are
pinned by `Cargo.lock`.

## Supported versions

The latest release on `main`. Report against the commit you are running (`lz --version`).
