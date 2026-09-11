---
name: performance
description: Speed up code by measuring: profile, fix top hotspot, verify numbers
---
# Performance

1. **Measure before changing.** Get a reproducible benchmark or timing (`hyperfine`, the language's profiler, request timings). Record the baseline number.
2. **Profile, don't guess.** Find the top hotspot (CPU profile, query plan, allocation count, network waterfall). Fix the biggest item only.
3. **Prefer algorithmic wins**: fewer passes, better data structure, batching I/O, caching pure results, avoiding repeated parsing/allocation in loops, pushing filters into the database.
4. **Keep correctness**: run the tests after each optimization; add a test for any edge case the faster path handles differently.
5. **Verify with the same benchmark** and report before/after numbers. Stop when the target is met — do not micro-optimize code that is not on the profile.
