# Infrastructure Galaxy

## Goal

Show servers, databases, providers and devices as functional objects inside the Observatory.

## Nodes

- VPS;
- Home Node;
- AI node;
- PostgreSQL;
- Redis;
- object storage;
- Docker services;
- IBKR gateway;
- MT5 node;
- Ollama;
- OpenAI;
- Claude;
- DeepSeek;
- Polymarket;
- trusted laptops/desktops.

## Metrics

- online/offline;
- CPU/RAM/GPU;
- disk;
- temperature;
- power estimate;
- network latency;
- service/container health;
- backup freshness;
- certificate expiry;
- provider quota/budget;
- last successful sync.

## Visual status

- stable orbit: healthy;
- wobble/pulse: degraded;
- red ring: critical;
- broken connection: offline;
- countdown ring: API quota reset;
- shrinking budget arc: remaining provider allowance.

## Data source

Use OpenTelemetry/Prometheus-style metrics and typed Jarvis events. The visualization is not the monitoring source of truth.
