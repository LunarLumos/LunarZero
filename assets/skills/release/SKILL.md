---
name: release
description: Cut a release: version bump, changelog, tag, artifacts, verify
---
# Release

1. Confirm a clean tree on the release branch and green tests/lint.
2. Decide the version (semver: breaking → major, features → minor, fixes → patch) from the commits since the last tag (`git log <last-tag>..HEAD --oneline`).
3. Bump the version everywhere it is declared (manifest, lock file, docs, constants) in one commit.
4. Update the changelog: grouped Added / Changed / Fixed / Removed, user-facing wording, link issues/PRs; move "Unreleased" under the new version with the date.
5. Build the artifacts the project ships and smoke-test them (`--version`, one real command).
6. Tag (`vX.Y.Z`, annotated) only when the user asks; list what to push and any publish command (crates.io, npm, PyPI, container registry) instead of running it unasked.
