---
name: codebase-map
description: Map an unfamiliar repo: layout, entry points, build/test commands
---
# Mapping a codebase

Spend a few tool calls, in this order, before answering questions about a new repo:
1. `read` the top-level README and any `AGENTS.md`/`CONTRIBUTING.md`; `glob` the root for manifests (`package.json`, `Cargo.toml`, `pyproject.toml`, `go.mod`, `Makefile`, CI config) to learn the build, test and lint commands.
2. `glob` the source tree two levels deep to see the module layout; note generated or vendored directories to ignore.
3. Find entry points: `grep` for `fn main`, `if __name__`, `createServer`, route registrations, CLI parsers.
4. Follow one request or command end-to-end to learn the layering (handler → service → storage) and error-handling style.
5. Skim two existing tests to learn the testing conventions.
Write down (for yourself) the three commands to build/test/run and the two or three conventions that everything follows; then answer or plan against that map, citing `path:line`.
