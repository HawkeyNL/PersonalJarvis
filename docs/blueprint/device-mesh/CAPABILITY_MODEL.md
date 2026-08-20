# Capability model

## Levels

### Level 0 — observe

- health;
- presence;
- logs;
- status.

### Level 1 — low-risk control

- launch approved application;
- restart approved local service;
- Wake-on-LAN;
- refresh agent.

### Level 2 — administrative

- deploy;
- edit scoped configuration;
- git push;
- package update;
- remote screen session.

Requires explicit confirmation.

### Level 3 — critical

- broker/wallet actions;
- production data deletion;
- unrestricted shell;
- secret access;
- security configuration changes.

Requires step-up authentication and narrowly scoped one-time authorization.

## Rule

An AI agent can request a capability. Only policy, approval and the Device Agent can authorize and execute it.
