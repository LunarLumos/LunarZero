---
name: refactoring
description: Restructure without behaviour change, small steps, tests green
---
# Refactoring

- Establish a safety net first: make sure the affected code has tests that pass; add a characterization test if it doesn't.
- Move in small steps that each keep the build and tests green: rename → extract → move → simplify. Commit (if asked) between steps.
- Preserve behaviour exactly, including error messages and edge cases, unless the user asked for a behaviour change — keep those as separate changes.
- Update every call site and reference (`grep` for the old name, including docs, configs and tests). Remove the old code; do not leave both paths.
- Keep the diff reviewable: no drive-by reformatting or unrelated cleanups.
- Finish with the full test run and a short summary of what moved where.
