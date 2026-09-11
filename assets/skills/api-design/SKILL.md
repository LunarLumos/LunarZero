---
name: api-design
description: HTTP/JSON API design: resources, errors, versioning, validation
---
# API design

- Model resources as nouns with plural paths (`/users/{id}/orders`); use verbs only for actions that are not CRUD (`/orders/{id}/cancel`).
- Use HTTP methods and status codes by meaning: 200/201/204 success, 400 validation, 401 unauthenticated, 403 forbidden, 404 missing, 409 conflict, 422 semantic error, 429 rate limit, 5xx server. Never return 200 with an error body.
- One error shape everywhere: `{ "error": { "code": "...", "message": "...", "details": [...] } }`; codes are stable strings, messages are for humans.
- Validate at the boundary; reject unknown fields only when the API is strict by policy. Paginate lists (cursor preferred); cap page size; document defaults.
- Keep responses stable: add fields, never rename or change types; version when you must break (`/v2` or a header).
- Idempotency for anything retried (PUT, idempotency keys for POST). Timestamps in RFC 3339 UTC, ids as strings.
- Follow the project's existing router, serializer and auth middleware — read two existing endpoints first.
