# Jarvis Public API, Core Routing & Home Node Ingress — AI Implementation Brief

## Purpose

This document defines the next architecture direction for PersonalJarvis. It is an implementation brief for Codex/other coding agents. Read the current repository, `AGENTS.md`, `jarvis-core/Jarvis.md`, existing ADRs, deployment docs, security hardening docs, and current API/Core code before changing anything.

Do not blindly copy this document into code. Reconcile it with the current implementation and preserve existing security guarantees.

The main decisions are:

- Jarvis clients connect directly to the UM890 Home Node over the public internet.
- RivetLink is only for remote desktop/screen takeover and is not part of Jarvis API networking.
- There is no mandatory VPS relay in the normal architecture.
- Public ingress is HTTPS/WSS on TCP 443 through a domain/DNS name and TLS.
- The internal Jarvis API/Core remains bound to localhost/private interfaces.
- The client sends a normal Jarvis message; Jarvis Core decides which specialist agent/tool/orchestrator should handle it.
- The app does not select a trading/coding/system agent itself.
- Realtime events are first-class so longer tasks can report progress and complete asynchronously.
- Monitoring is native to Jarvis; do not add Grafana by default.
- Existing tag-based release/update/rollback infrastructure must be reused and extended, not replaced.

---

## 1. Target network architecture

```text
Jarvis App
macOS / iOS / Windows / Linux
        |
        | HTTPS + WSS, TLS 1.3 preferred
        v
api.<jarvis-domain>
        |
        v
DNS -> public Home IP
        |
        | TCP 443 only
        v
Home router / firewall
        |
        v
UM890 reverse proxy / ingress gateway
        |
        | localhost/private upstream only
        v
127.0.0.1:8080 Jarvis API/Core
        |
        +--> identity / device auth
        +--> policy
        +--> orchestrator
        +--> agent runtime
        +--> memory/data store
        +--> trading/research/tools
```

Public exposure must not include:

- SSH
- PostgreSQL/SurrealDB or any database port
- Docker socket/API
- Cua/computer-use service
- TradingView/MT5 ports
- monitoring/admin endpoints
- raw Jarvis internal API port
- RDP/VNC/RivetLink internals

Only the deliberate HTTPS/WSS ingress should require public exposure.

If the Home IP is dynamic, design for secure dynamic DNS update. Do not hard-code the WAN IP in applications.

---

## 2. TLS and transport security

Use a publicly trusted TLS certificate with automatic renewal. Prefer a simple, well-supported reverse proxy or ingress implementation rather than custom TLS code unless the repository already contains a justified alternative.

Requirements:

- TLS 1.3 preferred; secure TLS 1.2 compatibility only if needed.
- HTTPS for request/response APIs.
- WSS or an equally secure event stream for realtime communication.
- HSTS once deployment is stable and correct.
- No plaintext public fallback.
- Certificate renewal must be automated and observable.
- Private application secrets must not be stored in the reverse-proxy configuration when avoidable.
- The proxy must overwrite untrusted forwarding headers instead of forwarding arbitrary client-supplied `X-Forwarded-For` values.
- Core must only trust forwarded client information from explicitly configured trusted proxy hops/IPs.

TLS protects transport, but it does not replace Jarvis application authentication.

---

## 3. Device-bound application authentication

Retain and strengthen the existing device-bound identity model.

Conceptual flow:

```text
registered device private key
        |
        | signs server challenge
        v
Jarvis API verifies registered public key
        |
        v
short-lived authenticated session
```

Requirements:

- Each device has its own identity/keypair.
- Private device keys never leave the client device.
- Revocation is per-device.
- Sessions/tokens are short-lived and scoped.
- Reauthentication/refresh must not silently weaken device binding.
- Existing signed approval/nonces/replay protections remain authoritative for privileged actions.
- Authentication, authorization, policy and trading risk approval remain separate layers.

Do not treat a valid TLS connection, IP address or possession of the DNS hostname as authentication.

---

## 4. API responsibility versus Core responsibility

The network/API layer must stay intentionally thin.

The API is responsible for:

- transport
- request validation
- authentication
- rate limiting
- request IDs/correlation IDs
- audit entry points
- serialization/versioning
- realtime connection lifecycle
- routing requests into Jarvis Core
- returning Core results/events

The API must NOT contain business routing such as:

```rust
if message.contains("trading") {
    trading_agent(...)
}
```

Agent/tool selection belongs in Jarvis Core/orchestration.

Desired separation:

```text
Client
  |
  v
API transport
  |
  v
Jarvis Core
  |
  +--> understand intent/context
  +--> retrieve relevant memory/state
  +--> classify risk/capability
  +--> select agent/orchestrator/tools
  +--> execute/coordinate
  +--> synthesize one Jarvis response
```

---

## 5. Chat API design

From the client perspective, sending a chat message should remain simple.

Example conceptual request:

```http
POST /v1/conversations/{conversation_id}/messages
```

```json
{
  "text": "Wat denk je van NQ vandaag?",
  "client_message_id": "...",
  "input_mode": "text"
}
```

The client must not need to know the selected agent.

Do not expose internal implementation details such as `agent=trading` as required client parameters.

Core should be able to decide:

```text
ordinary/general question -> general/research path
trading question          -> trading orchestrator
coding question           -> coding specialist path
Home Node/system question -> system/operations path
personal context question -> memory/context path
mixed question            -> multi-agent plan if justified
```

The final answer still comes from Jarvis as one assistant identity.

---

## 6. Synchronous and asynchronous conversations

Support the simple case and the long-running case.

### Fast/simple request

A short request may complete within one normal HTTP lifecycle.

```text
POST message
   -> Core
   -> answer
   -> 200 response
```

### Long-running request

Do not keep the architecture dependent on an indefinitely open HTTP request.

Recommended shape:

```text
POST message
   -> message/run accepted
   -> message_id / run_id

Realtime channel (WSS or SSE)
   -> run.started
   -> run.progress
   -> run.tool_activity (sanitized)
   -> assistant.partial (optional)
   -> assistant.completed
   -> run.failed / cancelled
```

This is required for Jarvis to behave naturally when research or agent work takes longer than expected.

Jarvis should be able to communicate progress such as "dit duurt iets langer" without inventing completion or blocking the client indefinitely.

Do not leak chain-of-thought or sensitive raw tool payloads in progress events. Report concise, user-safe status only.

---

## 7. Rust crate/module direction

Adapt to the repository rather than performing a gratuitous rewrite, but aim for clear boundaries comparable to:

```text
jarvis-api/
  public transport / auth / DTOs / realtime

crates/core/ or equivalent top-level runtime
  Jarvis request lifecycle and orchestration entry point

crates/orchestrator/
  planning, specialist selection, coordination, synthesis

crates/registry/
  agents, tools, capabilities and metadata

crates/policy/
  canonical authorization/risk/capability decisions

crates/agent/
  controlled execution runtime

crates/memory/ (when implemented)
  retrieval, context selection, persistence abstraction

crates/trading/ (when justified)
  trading analysis orchestration and domain logic

crates/events/ (or equivalent)
  typed Core/app events and subscriptions

crates/speech/
  speech providers / server-side voice capabilities where applicable
```

Do not add crates solely to match this list. Create a crate/module only when it provides a real ownership boundary.

Prefer typed Rust request/event structures over unstructured JSON blobs internally.

---

## 8. Agent routing principles

Agent selection is an internal Jarvis decision.

The Core should consider:

- user intent
- conversation context
- relevant memory
- requested capability
- risk class
- latency budget
- cost budget
- privacy requirements
- current system/tool availability

Example trading question:

```text
User message
   |
   v
Jarvis Core
   |
   +--> intent = trading analysis
   +--> retrieve portfolio/context if relevant
   +--> select Trading Orchestrator
             |
             +--> market data
             +--> news/research
             +--> technical specialist
             +--> fundamentals where relevant
             +--> bull/bear/reviewer pattern where useful
             +--> synthesize
   |
   v
Jarvis response
```

A trading question is not the same thing as a trading order.

If an action/order is requested, execution still flows through policy, deterministic risk controls, signed approval where required, and the trading gateway. No conversational agent may bypass that path.

---

## 9. Client presentation and voice

Core should return semantic response information; the client should own device-specific presentation decisions.

Example conceptual response metadata:

```json
{
  "text": "...",
  "presentation": {
    "speakable": true,
    "priority": "normal"
  }
}
```

The client knows whether:

- headphones/earbuds are connected
- voice output is enabled
- the app is foreground/background
- the platform permits immediate audio playback

Therefore do not make Core detect Mac/iPhone headphone state through the public API.

If voice is enabled, the client can request/use the configured TTS path and play Jarvis' response naturally.

---

## 10. Native Jarvis monitoring — no Grafana by default

Do not add Grafana as the default monitoring UI.

Jarvis should expose its own operational state to the Jarvis app.

Collect at least:

- CPU utilization/load
- memory and swap pressure
- disk capacity
- disk/SSD health where available
- temperatures
- network state/throughput/errors as appropriate
- Jarvis Core health/restarts
- database health
- Docker/service health
- GUI session health when relevant
- updater/release health
- backup age/status
- Cua/TradingView/MT5 process health when those components exist

The app can then render a native Jarvis monitoring interface.

### Thresholds should trigger investigation, not blind remediation

Bad design:

```text
CPU > 90% -> kill process
```

Desired design:

```text
threshold sustained/exceeded
        |
        v
system anomaly event
        |
        v
Jarvis investigates context
        |
        +--> expected workload (e.g. backtest) -> record/no alert
        |
        +--> unexpected condition -> explain cause / notify
                                 |
                                 +--> remediation proposal
                                        |
                                        v
                                   policy/approval
```

Jarvis must not automatically kill processes, delete data or restart privileged services solely because a threshold fired.

Implement a lightweight collector/rule engine before introducing a heavy metrics stack. Add Prometheus or similar only if measured requirements justify it later.

---

## 11. Release and automatic deployment integration

The repository already contains a tag-based Jarvis Core release and Home Node updater design. Reuse it.

The desired operator experience is:

```text
reviewed code merged to main
        |
        v
create stable tag vMAJOR.MINOR.PATCH
        |
        v
CI/release workflow
  fmt -> clippy -> tests -> audit -> updater safety tests
        |
        v
signed/trusted GitHub release context
archive + manifest + checksum
        |
        v
UM890 updater discovers release over outbound HTTPS
        |
        v
verify checksum/layout/version/migration compatibility
        |
        v
atomic release switch
        |
        v
restart affected Jarvis service(s)
        |
        v
/livez + /readyz
     /         \
 success      failure
   keep       rollback binary release
```

Requirements:

- Do not give GitHub Actions direct SSH credentials to the Home Node merely to deploy Core.
- Home Node should pull reviewed releases.
- Preserve immutable release directories and atomic switch behavior.
- Preserve readiness rollback.
- Preserve safe handling of database migration incompatibility.
- Extend the release artifact/configuration only where new API/ingress components genuinely require it.
- The public reverse proxy should have its own safe configuration/update lifecycle rather than being casually overwritten with every Core build.

---

## 12. Public ingress rollout order

Do not expose the Home Node immediately.

Implement and verify in this order:

1. Core/API works locally on `127.0.0.1`.
2. Device authentication and sessions are tested locally.
3. Realtime event channel is tested locally/LAN.
4. Trusted-proxy/IP attribution logic is tested.
5. Reverse proxy + TLS is configured on LAN/staging where possible.
6. DNS/DDNS automation is verified.
7. Firewall is default-deny and explicitly permits only required ingress.
8. External security tests are run against the public entry point.
9. Only then enable router/public exposure for TCP 443.

Never fix connectivity by binding all internal services to `0.0.0.0`.

---

## 13. Security tests required

Add/maintain regression tests for:

- unregistered device denial
- revoked device denial
- expired/replayed challenge denial
- expired/replayed approval nonce denial
- rate limiting
- request/body limits
- malformed chat/event payloads
- trusted versus spoofed forwarding headers
- unauthorized conversation access
- cross-device/session isolation
- Core policy decision consistency
- trading analysis versus trading execution separation
- event stream authorization
- event stream disconnect/reconnect behavior
- no secret/raw tool leakage through progress events

Public ingress changes must not weaken existing Core/sandbox/policy protections.

---

## 14. Implementation strategy for Codex

Do not implement this entire document in one PR.

First inspect current `main` and produce a short gap analysis. Then split work into small reviewable PRs, likely along lines such as:

1. ADR/docs update for direct public Home Node ingress and RivetLink separation.
2. Typed Core request/response boundary and chat lifecycle cleanup.
3. Realtime event abstraction and authenticated WSS/SSE transport.
4. Reverse-proxy/trusted-proxy configuration/runbook.
5. Native system metrics collector + anomaly events.
6. Jarvis app monitoring API contracts/UI work separately.
7. Release/updater adjustments only where required.

Keep each change independently testable and preserve CI green.

---

## Definition of done

This architecture is implemented when:

- a Jarvis client can securely reach the UM890 through a DNS hostname over HTTPS/WSS;
- only the deliberate public ingress is internet-facing;
- the client can submit one ordinary Jarvis chat message without choosing an agent;
- Core selects the appropriate specialist/orchestrator and returns one Jarvis response;
- longer tasks report safe progress and complete asynchronously;
- device-bound authentication and policy remain enforced;
- trading execution cannot bypass risk/approval controls;
- the Jarvis app can obtain native operational metrics without Grafana;
- anomalies cause investigation before remediation;
- a stable Git tag can produce a reviewed release that the UM890 safely auto-updates to with existing verification/rollback guarantees;
- RivetLink remains an entirely separate remote-desktop product/path.
