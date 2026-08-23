# Changelog

## Unreleased
- Added the fail-closed `jarvis-sandbox` provider boundary and ADR-040, with an
  authenticated loopback-only OpenSandbox control-plane candidate, bounded
  command/output protocol, controlled task input/artifact paths, and a patched
  DNS-to-nft path that rejects private/special-use destinations. Core wiring
  remains disabled until Ubuntu adversarial isolation is proven.
- Prevented the trusted OpenSandbox provider from honoring process proxy
  environment variables, so its loopback API credential cannot be routed to an
  accidental HTTP proxy.
- Hardened the OpenSandbox egress sidecar with the same PID and
  no-new-privileges constraints as workloads, while retaining only the network
  capability needed to enforce its nft rules.
- Removed the floating `uv` installation script from the control-plane build;
  the builder now requires a verified immutable `uv` OCI image digest.
- Made the OpenSandbox systemd unit reject missing or mutable Python/uv build
  image references before it invokes Docker Compose.
- Restricted the OpenSandbox control-plane key file to `root:root` mode `0600`;
  Jarvis Core no longer shares group read access to that credential.
- Added fixed CPU and memory ceilings to every OpenSandbox egress sidecar.
- Disabled the legacy direct host-process Claude Code executor so it cannot
  bypass the pending OpenSandbox/Codex execution boundary.
- Added CI checks for OpenSandbox control-plane template invariants and for
  applying the security patches to the pinned upstream source before running the
  private-DNS egress regression test.

## v3.0 — 5 augustus 2026
- Architecture Research en Codebase Impact Agents toegevoegd.
- Onafhankelijke reviewers en Fix Agent-loop toegevoegd.
- Engineering Memory, Observability Intelligence en Incident Learning toegevoegd.
- JAR-060 t/m JAR-075 toegevoegd.


## v2.9 — 5 augustus 2026
- Code Agent Constitution toegevoegd.
- Taalagents en Security Review Agent toegevoegd.
- Public API security, rate limiting, credential storage, access-control matrix en inputvalidatie vastgelegd.
- JAR-050 t/m JAR-057 toegevoegd.


## v2.7 — 4 augustus 2026
- Personal AI Operating System visie toegevoegd.
- Jarvis Philosophy toegevoegd.
- Life Domains en missie toegevoegd.

## v2.6 — 4 augustus 2026

Added:

- headless Home Node architecture;
- SSH, VPN and optional Cockpit management;
- trusted-device enrollment and capability model;
- typed remote tasks and remote-screen design;
- Home Node hardware recommendation and purchase checklist;
- API Quota Guardian with reset-aware pause/resume;
- Infrastructure Galaxy for servers, devices and provider budgets;
- JAR-150 through JAR-158 and JAR-920 through JAR-924.


## v2.5 — 4 augustus 2026

Added:

- Agent Observatory;
- 3D AI solar-system visualization;
- agent-agent and agent-tool message animation;
- live event stream and replay;
- cost, latency, performance and security modes;
- mobile battery-saving and 2D fallback;
- JAR-270 through JAR-278 tasks.


## v2.4 — 4 augustus 2026

Added:

- complete Jarvis memory architecture;
- PostgreSQL + pgvector deployment;
- Redis temporary-state policy;
- JSON/JSONB and compressed archive policy;
- encrypted client SQLite cache;
- memory consolidation and token-budget design;
- JAR-250 through JAR-257 tasks.


## v2.3 — 4 augustus 2026

- Event Alpha Engine toegevoegd.
- Polymarket-, news- en on-chain latency research toegevoegd.
- Shadow/paper-first gates en kostenmeting toegevoegd.


## v2.2 — 3 augustus 2026
- Afvinkbare roadmap in hoofd-README
- Centrale `TODOS.md`
- `STEPS.md`
- `STATUS.md`
- `DECISIONS_PENDING.md`
- Coding-agent werkregels
- Economics/cost tracking
- Crypto/prediction-market onderzoeksfase

## v2.1
- NautilusTrader evaluatie toegevoegd
