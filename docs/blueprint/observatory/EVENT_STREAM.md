# Observatory Event Stream

## Bron

De backend publiceert typed observability-events via WebSocket of SSE.

## Event envelope

```json
{
  "event_id": "evt_...",
  "trace_id": "trace_...",
  "run_id": "run_...",
  "timestamp": "2026-08-04T19:00:00Z",
  "event_type": "agent.message.sent",
  "source": {
    "type": "agent",
    "id": "news-agent"
  },
  "target": {
    "type": "agent",
    "id": "event-alpha-agent"
  },
  "status": "success",
  "summary": "Verified market-moving event",
  "metrics": {
    "latency_ms": 82,
    "input_tokens": 340,
    "output_tokens": 91,
    "estimated_cost_eur": "0.00041"
  },
  "sensitivity": "financial",
  "payload_ref": "obs_payload_..."
}
```

## Eventtypes

### Workflow

- `workflow.started`
- `workflow.completed`
- `workflow.failed`
- `workflow.cancelled`

### Agents

- `agent.started`
- `agent.message.sent`
- `agent.message.received`
- `agent.completed`
- `agent.failed`
- `agent.waiting`

### Models

- `model.request.started`
- `model.stream.chunk`
- `model.request.completed`
- `model.fallback.used`
- `model.budget.exceeded`

### Tools/MCP

- `tool.call.started`
- `tool.call.completed`
- `tool.call.failed`
- `mcp.server.connected`
- `mcp.schema.changed`

### Trading/security

- `risk.evaluation`
- `order.proposed`
- `approval.requested`
- `approval.confirmed`
- `execution.submitted`
- `execution.reconciled`
- `kill_switch.activated`

### Memory

- `memory.retrieved`
- `memory.created`
- `memory.superseded`
- `memory.consolidated`

## Privacy

De eventstream bevat standaard alleen summaries en metadata. Volledige prompts, toolpayloads en secrets worden niet naar de visualisatie gestuurd.
