---
name: testing
description: Write focused deterministic tests with the project's own runner
---
# Testing

- Use the project's existing framework and runner (look at how current tests are written and run; do not introduce a second one).
- One behaviour per test, named after the behaviour (`rejects_expired_token`, not `test1`). Arrange → act → assert; assert on outputs and effects, not on internals.
- Cover the boundary cases that matter: empty, one, many, maximum, invalid, concurrent where relevant. Skip cases that only restate the implementation.
- Prefer real objects over mocks; mock only I/O you cannot control (network, clock, randomness). Make time and randomness injectable rather than sleeping.
- Keep tests deterministic and independent of ordering or leftover files; use temp directories.
- When a test fails after your change, decide honestly whether the test or the code is wrong before editing either.
- Run the narrowest command first (single file/test), then the full suite once at the end. Report exact pass/fail counts.
