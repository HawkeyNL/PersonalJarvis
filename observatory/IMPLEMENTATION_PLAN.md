# Implementatieplan

## Fase O1 — typed events

- observability event schema;
- trace/run IDs;
- backend eventbus;
- PostgreSQL event persistence;
- WebSocket/SSE endpoint.

## Fase O2 — 2D prototype

Bouw eerst een eenvoudige 2D graph:

- Jarvis;
- agents;
- tools;
- lijnen;
- timeline;
- live events.

Doel: dataflow en UX valideren zonder 3D-complexiteit.

## Fase O3 — 3D prototype

- Three.js scene;
- vaste orbit-layout;
- agentnodes;
- message particles;
- camera controls;
- selection panel.

## Fase O4 — replay

- opgeslagen events laden;
- deterministic playback;
- speed controls;
- scrubber;
- snapshots.

## Fase O5 — metrics modes

- cost;
- latency;
- security;
- errors;
- tokengebruik.

## Fase O6 — mobiele optimalisatie

- quality levels;
- 2D fallback;
- battery saver;
- reduced motion;
- lifecycle pause when app is backgrounded.

## Fase O7 — production hardening

- payload redaction;
- permissions;
- retention;
- performance limits;
- E2E tests;
- failure/degraded modes.

## Definition of Done

- live agentrun zichtbaar;
- Jarvis-agent-tool roundtrip zichtbaar;
- agent-agent communicatie zichtbaar;
- kosten/tokens/latency zichtbaar;
- replay van historische run;
- privacyredactie getest;
- mobiele fallback werkt;
- visualization beïnvloedt agentuitvoering niet.
