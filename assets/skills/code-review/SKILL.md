---
name: code-review
description: Review a diff for bugs, risks, missing tests; findings with fixes
---
# Code review

Read the whole diff plus enough surrounding code to understand each change. Report findings most severe first, each with `path:line`, why it matters, and a concrete fix.

Look for, in order:
1. **Correctness** — wrong logic, missed branches, off-by-one, error paths that swallow failures, races, resource leaks.
2. **Security** — untrusted input reaching shell/SQL/paths/HTML, secrets in code, weak auth checks, unsafe deserialization.
3. **Behaviour changes** — public API or data-format changes, migrations, backwards compatibility.
4. **Tests** — is the change covered? Do tests assert behaviour or just run code?
5. **Maintainability** — duplication, misleading names, dead code, comments that lie.

Do not comment on style the project's formatter enforces. Do not edit files during a review. End with one line on what is good.
