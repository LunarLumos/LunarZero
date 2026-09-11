---
name: debugging
description: Find a bug systematically: reproduce, bisect, fix cause, regression test
---
# Debugging

1. **Reproduce first.** Get the exact failing command, input and error text. If it cannot be reproduced, write the smallest script or test that triggers it before touching code.
2. **Read the error literally.** Stack traces point at the first frame *in this project*; go there, not to the library frame.
3. **Narrow the search.** Bisect: comment out / stub halves, add a targeted log or assertion, check recent changes (`git log -p --since=…` on the file). One hypothesis at a time; write it down in one line before testing it.
4. **Fix the cause, not the symptom.** A `try/except` or null-check that hides the failure is not a fix unless the caller is genuinely allowed to send that input.
5. **Prove it.** Turn the reproduction into a regression test that fails before and passes after. Run the full relevant test set once more.
6. **Report.** State the root cause in one sentence, the change, and what was verified.

Common causes to check early: stale build artifacts, environment/config drift, off-by-one at boundaries, timezone/locale, unhandled empty input, concurrency ordering, wrong working directory.
