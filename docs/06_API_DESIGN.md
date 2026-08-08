# API-ontwerp

## Stijl

- REST voor commands en queries.
- WebSocket of SSE voor agentstreaming, quotes, orders en jobs.
- OpenAPI als contract.
- UUID/ULID identifiers.
- RFC 7807 problem details.
- Idempotency header voor mutaties.
- Optimistic concurrency via version/ETag.

## Voorbeeldroutes

### Auth/device

- `POST /v1/auth/login`
- `POST /v1/devices/register`
- `POST /v1/approvals/{proposal_id}/challenge`
- `POST /v1/approvals/{proposal_id}/confirm`

### Portfolio

- `GET /v1/portfolio`
- `GET /v1/portfolio/history`
- `GET /v1/positions`
- `PUT /v1/allocation-targets`
- `POST /v1/brokers/{id}/sync`

### Research

- `GET /v1/news`
- `GET /v1/instruments/{id}/research`
- `POST /v1/research/reports`
- `GET /v1/reports/{id}`

### Trading

- `POST /v1/order-proposals`
- `POST /v1/order-proposals/{id}/validate`
- `POST /v1/order-proposals/{id}/approve`
- `POST /v1/order-proposals/{id}/submit`
- `POST /v1/orders/{id}/cancel`
- `GET /v1/orders/{id}`
- `POST /v1/kill-switch/activate`

### Backtests

- `POST /v1/backtests`
- `GET /v1/backtests/{id}`
- `GET /v1/backtests/{id}/metrics`
- `POST /v1/strategies/{id}/promote-to-paper`

### Agents

- `POST /v1/chat`
- `GET /v1/agent-runs/{id}/events`
- `POST /v1/agent-runs/{id}/cancel`

### Content

- `POST /v1/trends/scan`
- `POST /v1/content-ideas`
- `POST /v1/scripts`
- `POST /v1/renders`
- `POST /v1/publications/{id}/approve`

## Command envelope

```json
{
  "request_id": "req_...",
  "device_id": "dev_...",
  "expected_version": 12,
  "payload": {}
}
```

## Errorcategorieën

- validation
- authentication
- authorization
- policy_denied
- risk_denied
- approval_required
- stale_market_data
- broker_unavailable
- duplicate_request
- rate_limited
- dependency_failure
