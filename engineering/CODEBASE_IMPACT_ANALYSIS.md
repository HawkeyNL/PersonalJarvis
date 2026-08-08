# Codebase Impact Analysis

Analyze:

- dependency graph;
- public interfaces;
- schemas and migrations;
- configuration and permissions;
- clients and generated contracts;
- jobs, events, queues and caches;
- runtime callers/consumers;
- data backfill and rollback;
- CPU/RAM/disk/network;
- deployment order;
- monitoring and alerts;
- regression risks.

## Risk classes

- Low: local/internal, no public contract or persistence change.
- Medium: cross-module, dependency, API or reversible migration.
- High: auth, secrets, finance, execution, destructive data or distributed state.

Medium and high changes require independent reviews.
