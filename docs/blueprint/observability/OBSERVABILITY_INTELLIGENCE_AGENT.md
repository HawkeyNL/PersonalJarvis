# Observability Intelligence Agent

## Inputs

- OpenTelemetry traces;
- structured logs;
- metrics;
- slow-query data;
- container/service health;
- CI failures;
- API and queue latency;
- AI model latency/tokens/cost;
- device health;
- user-perceived timings.

## Detects

- recurring errors and crash loops;
- timeout/retry clusters;
- slow spans and queries;
- lock contention and queue waits;
- N+1 queries and large payloads;
- resource saturation;
- excessive LLM calls and cache misses;
- provider routing and cost inefficiencies.

## Output

Every improvement proposal contains:

- evidence;
- affected workflows;
- suspected root cause;
- confidence;
- recommended investigation/fix;
- expected benefit;
- risk;
- acceptance metric;
- responsible agent.

It may create TODOs and attach diagnostics. It may not deploy, restart production, change limits or alter live trading autonomously.
