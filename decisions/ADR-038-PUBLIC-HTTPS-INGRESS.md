# ADR-038 — Public HTTPS ingress for the Ubuntu Home Node

**Status:** Accepted — 22 August 2026

## Context

Jarvis clients must reach the Home Node from outside the private network without
making the Core API, SurrealDB, Codex, broker adapters, SSH, or Docker public.
The existing device-bound authentication and signed-approval boundaries remain
the application trust boundary; a reverse proxy must not replace them.

## Decision

The only public application ingress is Caddy on TCP 443. `jarvis-api` runs as
the unprivileged `jarvis` user and in production accepts only a loopback socket
(`127.0.0.1` or `::1`). Caddy terminates public TLS and proxies to
`127.0.0.1:8080`.

The Caddy configuration uses a configured bare hostname
(`JARVIS_PUBLIC_HOSTNAME`) and TLS-ALPN-01, with HTTP-01 disabled. This keeps
certificate validation and renewal compatible with an owner router that forwards
only TCP 443. Caddy's normal default overwrites untrusted incoming
`X-Forwarded-*` values; API configuration trusts forwarded client identity only
when the direct socket peer is explicitly `127.0.0.1` or `::1` and the hop count
is one.

`/livez` and `/readyz` may be public because they return only generic status.
Detailed dependency state remains authenticated. Browser CORS is intentionally
not opened: the native Jarvis clients use the Tauri HTTP plugin.

Production enrollment remains disabled. The first owner device is enrolled from
a trusted local/private administration session before public exposure; a later
pairing/invitation protocol needs a separate ADR and device-signature design.

## Alternatives rejected

- Directly expose `jarvis-api`: rejects central TLS, loopback isolation and
  safe forwarded-header provenance.
- Public SSH/VPN replacement or automatic UPnP: expands the attack surface and
  takes router control away from the owner.
- Static global API key: weakens the existing device-bound Ed25519 design.
- Caddy HTTP-01 on port 80: conflicts with the one-public-port deployment goal.

## Rollout and rollback

Validate Caddy before reload, keep the previous Caddyfile, and confirm local
`/livez` and `/readyz` before opening TCP 443. Disabling `caddy` removes public
ingress without changing Core or SurrealDB. The release updater continues to
update only the native Core binary and rolls it back on readiness failure.
