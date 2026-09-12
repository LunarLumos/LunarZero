# Architecture

Four crates, one direction of dependency:

```mermaid
flowchart LR
  schema["lz-schema<br/>types · events · config · EngineApi"]
  core["lz-core<br/>engine · router · tools · permissions · sessions · MCP · LSP · index"]
  tui["lz-tui<br/>terminal UI"]
  web["lz-web<br/>local portal"]
  cli["lz-cli<br/>the lz binary"]
  schema --> core
  schema --> tui
  schema --> web
  core --> cli
  tui --> cli
  web --> cli
```

The TUI and the portal never touch `lz-core` directly: they talk to the
`EngineApi` trait and subscribe to the event bus. One process hosts all of
them; the portal shares the engine the terminal is using, which is why a
permission answered in the browser resolves in the terminal.

## A turn

```mermaid
sequenceDiagram
  participant U as user
  participant R as runner
  participant X as router
  participant P as processor
  participant M as model
  participant T as tools
  U->>R: prompt (mode, agent, model)
  loop until the assistant stops without tool calls
    R->>R: read history, build Need (tools? vision? ~tokens, text)
    R->>X: pick(strategy, need, policy)
    X-->>R: model + reason
    R->>R: system prompt: core + env + project map + <symbols> + instructions + skills
    R->>P: process(step)
    P->>M: stream request
    M-->>P: text / reasoning / tool calls
    P->>T: run tool calls concurrently (permission asks inside)
    T-->>P: results (+ LSP diagnostics, formatter)
    P-->>R: outcome
    R->>R: heal? plan open? queued message? background result?
  end
```

Three things keep a turn honest after the model has "finished":

- **heal** — the last check command failed, or an edited file still has language-server errors → a hidden repair message and another step (`heal.max_rounds`).
- **plan** — the model's own todo list has open items and it did not ask a question → sent back to them (twice at most).
- **queued input** — a message the user typed mid-turn, or a background subagent's result, is already the newest user turn; the loop simply continues.

## The router

`lunar/auto` is a virtual model. Every request builds a `Need` and the router
picks a real model from the connected free-tier members.

```mermaid
flowchart TD
  need["Need: tools, vision, ~tokens, user text"] --> cls["classify → chat / coding / reasoning / long-context"]
  cls --> cand["candidates: connected pool models<br/>that support tools/vision and fit the context"]
  cand --> sticky{"sticky model still usable<br/>and no model ≥12 quality points better is free?"}
  sticky -- yes --> pick
  sticky -- no --> score["score each candidate"]
  score --> pick["best → Pick{model, reason}"]
  subgraph score_detail["score"]
    q["quality (static rank)"]
    s["speed = ½ prior + ½ measured TTFT/tps"]
    h["headroom in RPM/RPD/TPM/TPD windows"]
    f["− failures · task nudge · policy (prefer / avoid / local-first / optimize)"]
  end
  score_detail -.-> score
```

Every model has a ledger entry (persisted in `~/.local/state/lunarzero/quota.json`):
request timestamps, token counts, cooldown, failure streak, last error,
measured latency. `blocked()` explains, in words, why a model cannot serve a
request right now; `lz pool why` prints it.

Failure handling is deliberately not a retry loop:

```mermaid
stateDiagram-v2
  [*] --> Ready
  Ready --> Cooling: 429 (retry-after → exact wait; daily cap → UTC midnight;<br/>else backoff 1·2·4… min, capped at 1 h)
  Ready --> Cooling: 5xx / timeout (30 s·2ⁿ, capped at 15 min)
  Ready --> Cooling: 404 model gone (24 h) · 400/422 (30 min) · too large (3 min)
  Ready --> ProviderCooling: 401/403 — the whole provider, 1 h
  Cooling --> Ready: cooldown elapsed
  ProviderCooling --> Ready: 1 h
  Ready --> Ready: success resets the streak
```

When a request fails **before any output streamed**, the processor asks the
router for the next model immediately (excluding the ones already tried this
step) and rebuilds the request for it — that is the 2 ms failover in
`BENCHMARKS.md`. When *every* candidate is blocked, the router reports the one
that frees up soonest and the step waits for it (bounded), then explains
exhaustion instead of spinning.

## Permissions

```mermaid
flowchart LR
  defaults["built-in defaults"] --> agent["agent rules (build / plan / …)"]
  agent --> mode["permission mode<br/>manual · accept edits · auto · plan"]
  mode --> user["the user's own config, again"]
  user --> session["session rules"]
  session --> approved["'always' approvals (project-scoped)"]
  approved --> eval["evaluate(permission, pattern)<br/>last match wins · default ask"]
```

A rule is `{permission, pattern, action}`; patterns are anchored globs
(`*` → anything, `?` → one char, trailing ` *` also matches the bare command)
with regex metacharacters taken literally. `bash` patterns are the command's
*prefix* by arity (`git checkout main` → `git checkout`) so an "always" approval
never widens to the whole binary for dangerous commands (`rm -rf /` → `rm`).

Modes sit between the agent's rules and the user's config, and the user's
config is re-applied after them: a mode can tighten defaults but cannot
override an explicit allow or deny. A *forced* ask — installing third-party
code the user did not request — bypasses all of it and needs a human; with
nobody there (`--auto`) it is refused.

## Compaction

```mermaid
flowchart TD
  step["step finishes with token usage"] --> over{"total ≥ usable window − reserve?"}
  over -- no --> prune["prune: stale tool outputs > 40k seen<br/>old write/edit inputs stubbed<br/>reasoning never replayed"]
  over -- yes --> summ["hidden compaction turn:<br/>transcript (tool output cut at 2k) + previous summary → summary message"]
  summ --> tail["keep the recent tail verbatim"]
  tail --> next["next step reads summary + tail"]
  prune --> next
```

The window is the *routed* model's context, not the virtual `lunar/auto`
placeholder — the sidebar percentage and the overflow check use the same
number. Skill outputs are protected from pruning; everything the model may
still need to quote (the last three assistant steps' inputs) is kept intact.

## Index and diagnostics

```mermaid
flowchart LR
  ts["tree-sitter index<br/>definitions · references · skeletons<br/>(Rust, Py, JS/TS, Go, Java, C/C++, Ruby)"] --> sym["<symbols> block in the prompt"]
  ts --> tool["symbol tool"]
  lsp["language server on PATH<br/>started on first edit"] --> diag["diagnostics in every edit result<br/>+ repair round before any build"]
  lsp --> tool
  fmt["project formatter"] --> edit["edit / write / patch results"]
```

The index is refreshed by mtime and cached on disk; a step never waits on a
cold index (it takes what is there). The language server's readiness is
tracked through `$/progress`, diagnostics are matched to the document version
that was sent, and `workspace/symbol` + `references` back the `symbol` tool for
anything the index does not model.
