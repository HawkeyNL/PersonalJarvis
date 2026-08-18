# Jarvis Home Node — GUI, Remote Access & Computer-Use Plan

> **Datastorenoot (ADR-034):** PostgreSQL/pgvector-verwijzingen hieronder zijn
> voor Jarvis Core vervangen door private SurrealDB 2.6.

## Purpose

The UM890 Pro is the intended always-on Jarvis Home Node. It is not a conventional headless API server: it must run Jarvis Core, Docker services, trading applications, GUI automation, backtesting workloads, and remote administration.

The coding AI should implement this plan against the existing PersonalJarvis architecture. Inspect current code, ADRs, deployment configuration, and existing security boundaries before changing anything.

## Target architecture

```text
Users / devices
  ├── macOS
  ├── Windows
  └── Linux
        │
        │ RivetLink / private VPN
        ▼
UM890 Pro — Ubuntu Desktop LTS
  │
  ├── Jarvis Core — native systemd service
  │
  ├── GUI session
  │    ├── TradingView
  │    ├── MT5 / Wine where required
  │    ├── browser
  │    └── other GUI applications
  │
  ├── Docker Engine
  │    ├── PostgreSQL + pgvector
  │    ├── monitoring
  │    ├── workers
  │    └── future isolated services
  │
  └── controlled GUI automation
       └── Cua / equivalent computer-use layer
```

## 1. Operating system

Use Ubuntu Desktop LTS rather than Server because the node needs a real graphical environment for TradingView, MT5, Cua/computer-use and other GUI applications.

The node must still be administrable without a physical monitor, keyboard or mouse after installation.

Requirements:

- SSH available only through the private network/RivetLink path.
- Firewall enabled and default-deny where practical.
- No public exposure of SSH, RDP, PostgreSQL, Redis or privileged control APIs.
- Automatic security updates configured deliberately and documented.
- Time synchronization enabled.
- Hostname and stable node identity configured.

## 2. Jarvis Core

Jarvis Core remains a native Rust process managed by systemd.

Do not put the Core in the normal Docker application stack. The Core is the orchestrator that must remain able to inspect and recover supporting services when Docker or an individual service fails.

Minimum requirements:

- systemd service
- restart policy
- startup ordering for required host dependencies
- health/readiness reporting
- graceful shutdown
- structured logs
- resource limits where appropriate
- secure configuration loading
- no secrets embedded in the unit file

## 3. Docker services

Supporting infrastructure should run in Docker with pinned image versions and persistent named volumes.

Initial expected services:

- PostgreSQL
- pgvector support
- monitoring/metrics where justified
- background workers when introduced

Redis is optional. Do not add it merely because it is common. Add it only when an actual workload requires ephemeral coordination, caching, queues, presence, rate limiting at scale, or similar behavior.

Database data must survive container recreation.

## 4. GUI session without a physical monitor

The Home Node must support a persistent graphical session without requiring a physical display.

The implementation should use a supported Linux remote/virtual display strategy. Prefer a stable Ubuntu-supported desktop remote access mechanism over ad-hoc X11 hacks.

Requirements:

- GUI applications can remain running while no monitor is connected.
- A remote user can connect to the GUI securely through RivetLink/private networking.
- GUI automation can interact with the intended session.
- Disconnecting a remote viewer must not terminate critical trading applications.
- Reboots should recover the intended GUI session automatically where safe.
- The architecture must document X11/Wayland implications for computer-use tooling.

Do not expose RDP/VNC directly to the public internet.

## 5. RivetLink integration

RivetLink is the preferred remote access path when its current implementation supports the required security properties.

The desired flow is:

```text
Mac / Windows / Linux laptop
        │
        ▼
     RivetLink
        │ authenticated encrypted connection
        ▼
      UM890
        │
        ▼
 Ubuntu GUI / remote desktop service
```

Requirements:

- strong device authentication
- encrypted transport
- explicit device authorization
- revocation support
- no direct public RDP exposure
- connection/audit events without logging secrets
- least-privilege access to GUI versus administrative functions

Inspect RivetLink before implementing a duplicate VPN/remote-access system.

## 6. GUI applications

The node is expected to host applications such as:

- TradingView Desktop
- MT5 through Wine if still required
- browser(s)
- Cua/computer-use tooling
- future GUI-based research/trading tools

Applications should have dedicated configuration/data directories where possible.

Do not run GUI applications as root.

Secrets used by trading applications must not become available to unrelated agents or child processes.

## 7. Computer-use / Cua boundary

Computer-use must be treated as a privileged capability, not as unrestricted desktop access.

Desired flow:

```text
Jarvis Agent
   ↓
ComputerUse capability
   ↓
Policy check
   ↓
Target application/session allowlist
   ↓
Cua
   ↓
GUI application
```

The agent must not automatically receive unrestricted keyboard/mouse control over the entire operating system.

Requirements:

- application/session allowlist
- explicit capability classification
- screenshots and UI state treated as potentially sensitive data
- command/action timeouts
- action audit trail
- kill switch
- safe cancellation
- no access to system authentication dialogs unless explicitly authorized
- no arbitrary credential extraction
- no unrestricted file-system access through GUI applications

Trading actions must remain subject to the existing trading/risk/policy architecture. GUI automation must not become a backdoor around those controls.

## 8. Remote GUI and Jarvis GUI must coexist

Design for two conceptual users of the graphical session:

1. Jarvis/Cua operating approved applications.
2. The human operator observing or taking over through RivetLink.

Human takeover must be explicit and auditable.

If multiple GUI sessions are required, isolate them rather than letting an agent operate a user's personal desktop session unintentionally.

## 9. Trading applications

TradingView and MT5 are GUI tools, not the authoritative trading engine.

Preferred architecture:

```text
Jarvis / Trading Agent
       ↓
Trading proposal
       ↓
Risk / Policy Engine
       ↓
Trading Gateway
       ↓
Broker API
```

GUI automation may be used for observation or unavoidable GUI-only workflows, but it must not bypass the deterministic execution gateway for live trading.

## 10. SSH and administration

SSH is for administration and diagnostics, not for exposing the Home Node publicly.

Use RivetLink/private VPN as the network boundary.

Administrative commands should be auditable where they affect Jarvis infrastructure.

Avoid granting the Jarvis LLM arbitrary sudo access. Privileged operations should use narrow host capabilities or a narrowly scoped updater/guardian service.

## 11. Updates and recovery

The Home Node must support safe updates for:

- Jarvis Core
- Docker images
- configuration
- GUI tooling where practical

Update flow:

```text
update request
   ↓
preflight
   ↓
backup / known-good reference
   ↓
build or pull
   ↓
tests / health checks
   ↓
activate
   ↓
verify
   ├── success → retain
   └── failure → rollback
```

Do not let an LLM directly overwrite its own running binary or execute arbitrary root commands.

## 12. Monitoring

Monitor at least:

- CPU
- memory
- disk usage
- disk health where available
- temperatures
- network connectivity
- Docker health
- PostgreSQL health
- Jarvis Core health
- GUI session health
- TradingView/MT5 process health where relevant
- Cua health
- backup status

Jarvis should be able to inspect these metrics through typed capabilities rather than arbitrary shell execution.

## 13. Security requirements

This plan is subject to `docs/AI_SECURITY_HARDENING.md`.

In particular:

- GUI access does not bypass policy.
- Cua does not bypass policy.
- RivetLink does not bypass device authorization.
- Trading GUI actions do not bypass the Risk Engine.
- Secrets are never passed wholesale to agents.
- No public exposure of privileged services.
- Fail closed on authorization failures.
- Every privileged operation is auditable.

## 14. Implementation order

Implement in small, independently reviewable PRs:

1. Host deployment/systemd baseline.
2. Docker Compose production baseline for PostgreSQL/pgvector and required infrastructure.
3. Secure private-network SSH configuration/documentation.
4. GUI session/remote desktop baseline.
5. RivetLink integration after inspecting its existing protocol and security model.
6. GUI application lifecycle management.
7. Computer-use/Cua capability boundary.
8. Monitoring and health checks.
9. Update/rollback flow.
10. Trading-specific GUI integration only after the security/policy boundaries are proven.

Each PR should include tests or operational verification appropriate to the change and update ADRs when an architectural decision changes.

## Definition of done

The Home Node is ready when:

- it boots without a physical monitor;
- Jarvis Core starts and remains independently recoverable;
- Docker services recover safely;
- PostgreSQL data persists;
- GUI applications can run without a physical display;
- the user can securely reach the GUI from Mac, Windows and Linux through RivetLink/private networking;
- Jarvis can use Cua only through an explicit policy/capability boundary;
- GUI automation cannot bypass trading risk controls;
- monitoring detects failures;
- updates can be rolled back;
- no privileged service is exposed directly to the public internet.
