# Contributing

Thanks for looking. The fastest ways to help, in order of impact:

1. **Prompts the router gets wrong** — add lines to `assets/eval/routing.jsonl` and run `lz pool eval`.
2. **Skills and MCP servers people actually use** — add them to the curated catalog (below).
3. **Free-tier limits that are out of date** — `lz pool why <model>` shows what a provider really enforces; fix `scripts/build_pool.py` / `assets/pool.json`.
4. Bugs, with a `LZ_LOG_LEVEL=debug lz run --print-logs …` transcript when you can.

## Building

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
python3 scripts/bench.py target/release/lz   # optional: router benchmarks against the mock
```

CI runs the same three commands plus `cargo audit`. Please keep them green.

## Adding a skill or MCP server to the catalog

Edit [`assets/recommended.json`](assets/recommended.json). One entry, one line:

```jsonc
// MCP server
{ "alias": "short-name", "source": "npm:@scope/pkg" /* or pypi:pkg, or a GitHub URL */,
  "description": "What it gives the agent, in one line", "env": ["API_KEY_IT_NEEDS"], "args": ["extra", "args"] }

// skill
{ "alias": "short-name", "source": "https://github.com/owner/repo/tree/main/skills/name",
  "description": "What it teaches the agent, in one line" }
```

Review checklist for a catalog PR:

- [ ] The source is a public repository or registry package under an open licence.
- [ ] `lz mcp install <alias>` (or `lz skill install <alias>`) succeeds on a clean machine — say which OS you tried.
- [ ] For MCP servers: the tool list is reasonable in size (a server with 80 tools costs tokens on every turn it is loaded) and any required env vars are listed in `env`.
- [ ] For skills: `SKILL.md` has a `name` and a one-paragraph `description`; it does not tell the agent to install other things.
- [ ] Nothing in it phones home beyond what its description says.

A skill is a folder with a `SKILL.md`; see the ten in [`assets/skills/`](assets/skills/) for the shape.

## Code

- Match the surrounding code; short doc comments that say *why*, not what.
- A change to the permission engine, the router or the installers needs a test next to the existing ones in the same module.
- New tools go in `crates/lz-core/src/tool/builtins/` and get a line in the core prompt only if the model needs to know when to use them.
- User-facing behaviour changes get a line in `CHANGELOG.md`.

## Security

Please report exploitable issues privately — see [SECURITY.md](SECURITY.md).
