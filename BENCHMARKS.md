# Benchmarks

Numbers you can reproduce, and the data they come from. Nothing here is a
marketing estimate: every figure is either the output of a script in this
repository or a reading from a real usage ledger, and each says which.

## Routing classifier — `lz pool eval`

`lunar/auto` decides per request whether a prompt is chat, coding, reasoning or
long-context, and routes accordingly. The classifier is scored against
[`assets/eval/routing.jsonl`](assets/eval/routing.jsonl): 200 labelled prompts
(60 chat, 60 coding, 60 reasoning, 20 long-context). A unit test fails CI if
accuracy drops below 90%.

| class | accuracy |
|---|---|
| chat | 59/60 (98%) |
| coding | 59/60 (98%) |
| reasoning | 60/60 (100%) |
| long-context | 20/20 (100%) |
| **overall** | **99.0%** |

Caveats, stated plainly: the set is small and was written by the maintainer; the
classifier was tuned against it (the previous substring-based version scored
88.5% on the same set). The two remaining misses are shown by `lz pool eval`.
Pull requests adding prompts — especially ones the classifier gets wrong — are
the most useful contribution to this number.

## Router behaviour — `scripts/bench.py`

Run against the local mock provider (`scripts/mock_provider.py`); no key is used.
`python3 scripts/bench.py target/release/lz` on an Apple M-series laptop:

| measurement | result |
|---|---|
| failover after a 429, before any output streamed | **2 ms** |
| failover after a 503, before any output streamed | **2 ms** |
| 429 with `retry-after: 3` on the only available model | run waits **4.7 s** and answers (no backoff table, no failure) |
| six-step tool turn (write, edit, bash, glob, grep, todo) | 9 requests, **0.2 s** wall clock excluding model time |
| fixed overhead per agent step (system prompt + 11 tool schemas) | **~2.5k tokens** |
| full agent steps sent in that turn | 2.5k–3.3k tokens each |

The failover time is the gap between the failed request and the next request on
another model; it is dominated by the router's ledger update, not the network.

Binary: 26.7 MB release build with tree-sitter grammars for five languages
built in; `lz --version` in 11 ms.

## Live free-tier usage — `lz pool report`

From a real ledger (`~/.local/state/lunarzero/quota.json`) on the maintainer's
machine, 24 hours of building a Next.js application and this repository's
features with `lunar/auto`:

```
last 24 h — 468 requests · 14,726,458 tokens · 43 model(s) used
```

All on free tiers, $0 spent. `lz pool report` prints the per-model table and
what the same tokens would have cost at the providers' list prices; note that
several free endpoints (ollama-cloud, the `:free` OpenRouter models) publish no
price, so the "list $" column understates the comparison.

`lz pool why <provider/model>` explains a single model's state — limits used,
cooldown reason, last error, measured latency — which is also how we found that
one provider's real per-minute token limit is lower than the published figure.

## Not measured (yet)

Task-completion quality against SWE-bench-style suites, and multi-day effective
uptime of the pool. Both need infrastructure this project does not have; if you
run one, please share the setup.
