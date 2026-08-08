# Device Mesh Architecture

## Doel

Jarvis kan vertrouwde apparaten zien, bewaken en beperkte taken laten uitvoeren zonder ieder apparaat direct vanaf internet bereikbaar te maken.

```text
                         Internet
                             │
                       Jarvis VPS
                 API / Orchestrator / DB
                             │
                  outbound mTLS/VPN tunnel
                             │
                    Jarvis Home Node
                      headless, 24/7
               ┌─────────────┼─────────────┐
               │             │             │
        Linux desktop     MacBook      Windows pc/VPS
        Device Agent      Device Agent Device Agent
```

## VPS

- centrale identity;
- device registry;
- orchestration;
- approvals;
- audit;
- notifications;
- public API for clients.

## Home Node

- always-on bridge to the home network;
- device discovery;
- outbound-only secure connection;
- local task relay;
- local health monitoring;
- Wake-on-LAN where supported;
- SSH and web administration;
- optional Home Assistant/MQTT;
- no monitor, keyboard or mouse required.

## Device Agents

A managed laptop or desktop must be powered on and connected before it can execute a task.

When offline:

- Jarvis reports it as offline;
- no silent retries of sensitive commands;
- financial and destructive actions are never queued;
- safe informational tasks may optionally wait with an expiry.
