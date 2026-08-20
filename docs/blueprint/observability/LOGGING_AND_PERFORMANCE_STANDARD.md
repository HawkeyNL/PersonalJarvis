# Logging and Performance Standard

## Structured logging

Include timestamp, service, environment, version, trace/span/run IDs, event name, severity, duration, outcome and safe metadata.

Never log passwords, API keys, tokens, private keys, authorization headers, broker credentials or unnecessary personal/financial payloads.

## Performance engineering

1. Define budgets.
2. Measure p50/p95/p99 and critical path.
3. Reproduce/profile.
4. Form a hypothesis.
5. Benchmark before/after.
6. Check regressions.
7. Store results.

Track API latency, voice first response, memory retrieval, UI responsiveness, database queries, agent startup, event-stream delay and trading/risk critical paths.
