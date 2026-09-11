---
name: git-workflow
description: Safe git: branches, atomic commits, clear messages, no history rewrites
---
# Git workflow

- Check state before acting: `git status`, `git branch --show-current`, `git log --oneline -5`. Never assume a clean tree.
- Work on a branch for anything non-trivial; name it by intent (`fix/login-redirect`, `feat/export-csv`).
- Commit atomic units: one logical change per commit, message = imperative summary under 60 chars, blank line, then *why* (not what — the diff shows what). Follow the repo's existing convention (e.g. Conventional Commits) when there is one.
- Stage deliberately (`git add -p` or explicit paths). Never commit secrets, build output, or unrelated formatting churn.
- Never rewrite shared history, force-push, `reset --hard`, or discard changes unless the user explicitly asks; prefer `git revert` for published commits.
- Before a PR: rebase or merge the target branch, run tests, summarize the change and how it was verified; keep the PR focused.
- Only commit or push when the user asked for it.
