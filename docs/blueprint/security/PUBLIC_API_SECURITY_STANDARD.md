# Public API Security Standard

Applies to internet-facing HTTP, WebSocket, SSE, webhook, MCP and callback endpoints.

## Authentication and credentials
- Prefer short-lived access tokens, rotating refresh tokens, device-bound credentials, OAuth/OIDC and mTLS service identities.
- Static server secrets: Docker secrets, SOPS, Vault or root-only secret files.
- Dynamic external tokens: encrypted in PostgreSQL; master key outside the database.
- Refresh tokens: store only hashes, rotate per use and detect reuse.
- Clients store only device/session secrets in OS secure storage.

## Rate limiting
Every endpoint defines:
- per-IP, per-user/device and per-token limits;
- burst and sustained limits;
- concurrency, payload-size and timeout limits;
- weighted costs for expensive endpoints;
- `429` plus `Retry-After`;
- metrics and alerts.

Use stricter profiles for login, enrollment, approvals, AI calls, uploads and financial proposals.

## Access control
Every route/tool/task maps to a versioned capability in the Access Control Matrix.
Enforce server-side with deny-by-default, ownership checks, environment checks, trusted-device requirements and step-up approval where needed.

## Input validation
Require:
- explicit runtime schema;
- length/range/enum limits;
- canonical IDs;
- decimal and overflow checks;
- file type/size limits;
- URL/host allowlists and SSRF defenses;
- cross-field business validation;
- reject unknown fields for critical commands.

## Output and errors
No stack traces, secrets or unauthorized resource details. Escape untrusted text, bound response sizes and use stable error codes.

## Webhooks
Signed raw payload, timestamp, nonce, replay window, idempotency and asynchronous processing.

## Testing gate
Authentication, authorization-matrix, rate-limit, validation/fuzz, injection, SSRF/path traversal, replay/idempotency, secret-leak and oversized/slow-request tests are mandatory.
