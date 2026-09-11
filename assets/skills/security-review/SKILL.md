---
name: security-review
description: Audit code for injection, auth, secrets, unsafe deps; fix safely
---
# Security review

Check each item against the code that handles untrusted input (HTTP, CLI args, files, env):
- **Injection**: shell commands built from strings (use argument arrays), SQL without parameters, path joins without normalization (`../`), HTML/JS interpolation without escaping, regex from user input.
- **Auth & access**: every privileged route checks identity *and* authorization; tokens compared in constant time; sessions expire; no auth decisions on client-controlled fields.
- **Secrets**: none hard-coded or logged; loaded from env/secret store; `.gitignore` covers local config.
- **Data**: validate types, sizes and ranges at boundaries; limit upload/request sizes; safe defaults when a field is missing.
- **Crypto**: standard libraries only, modern algorithms, random from a CSPRNG.
- **Dependencies**: pinned, known versions; run the ecosystem's audit command if available.
- **Errors**: no stack traces or internal paths returned to clients.
Report findings with severity, location and fix. Only fix what the user asked to fix; never weaken a check to make a test pass.
