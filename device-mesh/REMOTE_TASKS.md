# Remote Tasks

## Flow

```text
User/Jarvis
→ typed task proposal
→ device availability check
→ capability policy
→ approval if required
→ signed command
→ Device Agent
→ result
→ audit
```

## Task envelope

```json
{
  "task_id": "task_...",
  "device_id": "dev_...",
  "capability": "docker.restart",
  "parameters": {
    "container": "market-data-worker"
  },
  "expires_at": "2026-08-04T20:30:00Z",
  "approval_id": "approval_...",
  "idempotency_key": "idem_..."
}
```

## Queue policy

Queueable:

- read health after reconnect;
- collect logs;
- refresh inventory.

Not queueable:

- trades;
- deletes;
- shell sessions;
- configuration changes;
- remote screen activation;
- secret operations.
