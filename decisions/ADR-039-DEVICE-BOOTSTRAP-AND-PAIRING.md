# ADR-039 — device bootstrap and remote pairing

## Status

Accepted.

## Decision

Jarvis has no global password, API key, or shared device secret.

- The first owner device is enrolled only through `POST /v1/auth/bootstrap` when there are no active trusted devices, the resolved client IP is in `JARVIS_BOOTSTRAP_ALLOWED_CIDRS`, and the request carries the single-use secret verifier configured as `JARVIS_BOOTSTRAP_SECRET_SHA256`.
- Caddy forwarding headers are used only when its loopback peer is explicitly configured; a direct caller cannot spoof `X-Forwarded-For`.
- Subsequent devices create a five-minute pending pairing request. An active owner device must pass its normal OS biometric/passcode gate and sign the canonical `jarvis-device-pairing-v1` payload binding request ID, nonce, candidate public key, owner, approver and expiry.
- The server verifies the signer is active and owner-bound, then conditionally consumes the pending request before activating the candidate key. A bearer session never substitutes for this signature.

## Recovery

Only a SHA-256 verifier is available to Core; the random raw bootstrap secret is generated and shown by a local root provisioning/recovery operation, is never committed or logged, and is removed after use. If all devices are lost, the owner must use local root access to rotate the verifier and explicitly reset the latch; there is no remote recovery route.
