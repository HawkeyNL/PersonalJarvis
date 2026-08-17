# SurrealDB Evaluation & Migration Plan — AI Engineering Brief

## Purpose

Evaluate whether PersonalJarvis should replace PostgreSQL + pgvector with SurrealDB 3.x as its primary datastore.

This document is intentionally an evaluation and migration-planning brief. Do **not** immediately remove PostgreSQL, rewrite persistence code, or migrate production data. First inspect the current repository and prove that SurrealDB is the better fit for PersonalJarvis with concrete evidence.

Read `AGENTS.md`, `core/Jarvis.md`, all persistence-related ADRs, migrations, SQLx usage, auth/audit code, trading/portfolio code, memory plans, deployment docs and release/update constraints before proposing changes.

---

## 1. Why SurrealDB is being considered

Jarvis is evolving beyond a conventional CRUD application. Its long-term data model includes:

- identities and devices
- conversations and messages
- long-term memories
- semantic embeddings
- entities and relationships
- knowledge/provenance
- agents, tools and runs
- operational events/metrics
- research data
- portfolio/trading state
- audit/security events

SurrealDB combines relational/document, graph, vector, full-text and realtime capabilities in one database and has an official Rust SDK. That may map more naturally to Jarvis than PostgreSQL + pgvector plus additional relationship/event abstractions.

This possible fit must be weighed against PostgreSQL's much greater operational maturity and ecosystem.

---

## 2. Compare the current PostgreSQL design against SurrealDB

Produce a concrete comparison for PersonalJarvis, not a generic database marketing summary.

Evaluate at least:

### Reliability and maturity

- transaction semantics
- crash recovery
- consistency guarantees
- corruption/failure scenarios
- production history
- known operational limitations
- upgrade compatibility
- tooling maturity

### Rust integration

Compare current SQLx/PostgreSQL usage against the official SurrealDB Rust SDK:

- typed data access
- async support
- transactions
- prepared/bound queries
- connection management
- error handling
- testing ergonomics
- migrations/schema management
- observability/instrumentation
- remote versus embedded modes

Prefer a remote standalone SurrealDB service for production unless there is a strong reason for embedding it into Core. Database lifecycle must remain separable from Jarvis Core restart/deployment lifecycle.

### Query/data model capabilities

Evaluate:

- relational-style structured records
- document data
- graph relationships/edges
- graph traversal
- vector indexes/search
- filtered semantic search
- full-text search
- hybrid retrieval
- live/realtime queries
- time-oriented/event data
- schemafull versus schemaless behavior

### Operations

Evaluate:

- backup
- restore
- tested disaster recovery
- export/import
- point-in-time recovery equivalents, if any
- replication
- high availability options
- monitoring
- upgrades
- data repair/recovery procedures
- Docker/self-host deployment on the UM890

### Ecosystem/support

Compare:

- official documentation
- vendor support
- community maturity
- release cadence
- issue responsiveness
- long-term maintenance risk
- compatibility guarantees
- third-party tooling

---

## 3. AI and LLM-specific capabilities

Evaluate the AI-specific advantages in the context of Jarvis.

Research and validate current SurrealDB support for:

- vector search/index types
- graph + vector retrieval
- hybrid structured/semantic retrieval
- AI/agent frameworks and integrations
- RAG use cases
- agent memory patterns
- provenance/trust/temporal memory concepts
- Spectron or other SurrealDB-native AI memory tooling

Do not adopt Spectron or any external memory framework simply because it exists. Determine which concepts are useful, which conflict with our own Core/memory design, and whether using the framework creates unwanted coupling.

A likely Jarvis memory model may need to express concepts such as:

```text
memory
  content
  embedding
  source
  provenance
  confidence
  created_at
  valid_from / valid_to
  user/person/project/entity relationships
  related memories/facts
  superseded beliefs
```

Assess whether SurrealDB materially simplifies this compared with PostgreSQL + pgvector.

---

## 4. Candidate Jarvis data model

If SurrealDB proves suitable, propose a schema design using deliberate `SCHEMAFULL`/strongly constrained models for security-critical and financial data.

Potential domains:

```text
identity
  users
  devices
  sessions/challenges

conversation
  conversations
  messages
  runs

memory / knowledge
  memories
  facts
  entities
  relationships
  embeddings
  provenance

agents
  agent definitions
  capabilities
  runs
  tool activity

system
  metrics
  anomalies/incidents
  service state
  release/update state

audit
  security events
  privileged actions

trading
  accounts
  portfolios
  positions/snapshots
  research
  proposals
  approvals
  execution records
```

Do not use schemaless design for monetary values, auth, security/audit or trading execution simply because the database permits it.

Money must remain represented with an exact decimal strategy, never floating point.

---

## 5. Graph model evaluation

Prove whether native graph relationships provide a meaningful advantage.

Example conceptual relations:

```text
user:gus --OWNS--> device:macbook
user:gus --WORKS_ON--> project:jarvis
conversation:x --MENTIONS--> asset:NQ
memory:y --ABOUT--> project:jarvis
fact:z --SUPPORTED_BY--> source:s
agent_run:r --USED_TOOL--> tool:market_data
```

Edges may need their own metadata such as timestamp, confidence, source and provenance.

Compare representative graph queries with equivalent PostgreSQL relational queries/recursive CTEs and measure both complexity and performance.

---

## 6. Vector/memory evaluation

Create representative semantic-memory workloads.

Queries should combine vector similarity with structured constraints, for example:

```text
semantic similarity to current question
AND owner = current user
AND memory_type = fact
AND confidence >= threshold
AND expired = false
AND related project = PersonalJarvis
```

Compare:

- exact search
- approximate indexes
- HNSW behavior
- other relevant SurrealDB vector index options
- filtered top-K correctness
- index build time
- query latency
- memory consumption
- insert/update throughput

Do not benchmark only unfiltered nearest-neighbor lookup; Jarvis retrieval will frequently combine semantic search with metadata/policy filters.

---

## 7. Jarvis-specific benchmark / proof of concept

Before approving migration, build a small isolated benchmark/POC that does not modify production persistence.

Test PostgreSQL + pgvector versus SurrealDB using representative data on the UM890 where practical.

Suggested datasets:

- 100k memories
- optionally 1M memories if inexpensive enough
- conversations/messages
- entities/graph edges
- audit/event records
- system metrics
- representative embeddings

Measure at least:

- cold/warm startup
- bulk insert throughput
- single-record writes
- concurrent reads/writes
- conversation-history queries
- filtered vector retrieval
- graph traversal
- hybrid graph/vector retrieval
- append-heavy audit/event workload
- storage footprint
- RAM usage
- CPU utilization
- backup time
- restore time

Use the same or comparable hardware/resources.

Record methodology and raw results so the decision is reproducible.

Do not claim SurrealDB is faster or slower globally from vendor benchmarks alone.

---

## 8. Security-critical persistence review

Pay special attention to existing features that are security-sensitive:

- device registration/identity
- challenges/nonces
- sessions
- login lockout/rate-limit persistence where applicable
- signed approvals
- nonce replay prevention
- security audit
- policy-relevant state

For each, prove:

- atomicity requirements
- uniqueness constraints
- race-safety
- replay prevention
- expiration handling
- transaction semantics
- crash behavior

Do not migrate auth/security persistence until equivalence tests pass.

---

## 9. Audit log requirements

The current security audit model must not become weaker.

Evaluate how to preserve an append-only operational model in SurrealDB.

Requirements:

- application has no ordinary update/delete path for audit records
- immutable event IDs
- reliable timestamping
- actor/device/session correlation
- structured event types
- no secrets in audit payloads
- retention/export strategy
- backup/restore verification

If SurrealDB cannot provide equivalent confidence to the current model, document that as a blocker or keep a more conservative persistence approach for audit.

---

## 10. Trading and financial data

Do not let AI-oriented database features reduce trading-data integrity.

Evaluate:

- exact decimal/money representation
- transactions around portfolio/account state
- idempotency keys
- execution records
- order/trade correlation
- immutable history
- concurrent update behavior

Trading execution remains controlled by deterministic risk/execution code and policy; the database choice does not alter that security boundary.

---

## 11. Realtime / live query opportunities

Evaluate SurrealDB Live Queries as an implementation tool, not as a reason by itself to migrate.

Potential uses:

- new system anomaly -> Core investigation trigger
- agent run status -> app realtime event
- conversation state changes
- monitoring dashboard updates
- job/task completion

Compare this with the existing/intended Rust event architecture. Avoid coupling client apps directly to the database; Core/API should remain the authorization boundary.

The Jarvis app must not connect directly to SurrealDB from the public internet.

---

## 12. Persistence abstraction

Do not scatter SurrealQL throughout every crate.

If migration is approved, define clear repository/store boundaries around domain persistence.

For example, conceptually:

```rust
trait ConversationStore { ... }
trait IdentityStore { ... }
trait MemoryStore { ... }
trait AuditStore { ... }
trait PortfolioStore { ... }
```

Do not invent one giant generic `JarvisStore` interface if domain-specific interfaces provide clearer ownership and testing.

The abstraction should make business/domain code testable without depending on raw database query syntax everywhere.

---

## 13. Deployment impact

The current Home Node design uses PostgreSQL in Docker and Core via systemd.

If SurrealDB is selected, preserve the same operational principle:

```text
Ubuntu host
  Jarvis Core -> systemd

Docker/private service
  SurrealDB
```

Requirements:

- DB binds only to localhost/private Docker network as appropriate
- no public database port
- durable storage
- explicit version pinning
- health checks
- backups independent of container lifecycle
- resource limits/monitoring
- secure DB credentials

Do not expose SurrealDB management/query endpoints to the public internet.

---

## 14. Release and migration compatibility

This is critical because Jarvis Core now has tag-based automatic release/update/rollback behavior.

The existing updater deliberately refuses automatic binary updates when SQL migration fingerprints change because binary rollback cannot safely reverse database migrations.

A SurrealDB migration system must preserve or improve this safety property.

Design:

- deterministic schema/migration fingerprint
- tagged release manifest includes persistence schema revision
- automatic updater only accepts schema-compatible releases
- schema-changing releases require deliberate backup + migration + recovery plan
- rollback behavior is explicitly documented

Do not weaken automatic update safety just to simplify migration tooling.

---

## 15. Migration plan if SurrealDB wins

Only after the evaluation/POC is approved, propose incremental migration steps.

A safe possible order:

1. Introduce persistence interfaces/tests without changing DB behavior.
2. Add SurrealDB development/test infrastructure.
3. Implement low-risk domain store in SurrealDB.
4. Build parity/integration tests.
5. Implement conversation/memory domains.
6. Implement identity/security only after race/transaction tests prove equivalence.
7. Implement audit with append-only guarantees.
8. Implement portfolio/trading persistence last.
9. Build export/import verification tooling.
10. Test backup + full restore on a disposable Home Node environment.
11. Cut over only after the complete CI/security/recovery gate is green.

Avoid an indefinite dual-write architecture. Temporary dual-read/verification may be acceptable if tightly scoped, but maintaining PostgreSQL and SurrealDB as permanent co-equal sources of truth should require a strong reason.

---

## 16. Decision criteria

Recommend SurrealDB only if the evaluation demonstrates that the benefits outweigh operational risk.

Strong reasons in favor may include:

- substantially simpler semantic + graph memory model
- simpler hybrid retrieval
- cleaner entity/relationship representation
- useful native realtime behavior
- excellent Rust ergonomics
- acceptable operational/recovery behavior on the Home Node
- equivalent guarantees for auth/audit/trading data

Reasons to retain PostgreSQL may include:

- weaker backup/recovery guarantees
- unacceptable transaction/race behavior for critical domains
- material stability issues
- insufficient migration/versioning safety
- operational complexity outweighing multi-model benefits
- benchmark results showing no meaningful Jarvis-specific benefit

It is acceptable to conclude that PostgreSQL + pgvector remains the better choice.

---

## 17. Deliverables for the evaluation PR(s)

Codex should first deliver a design/research result, not a full migration.

Produce:

- current PostgreSQL usage inventory
- SurrealDB feature/operational comparison
- proposed Jarvis SurrealDB schema/data model
- benchmark/POC design
- benchmark results if implemented
- security-critical parity analysis
- backup/restore test plan
- release/updater migration-safety design
- recommendation: migrate / remain on PostgreSQL / defer
- concrete incremental implementation plan

Clearly label assumptions and unverified claims.

---

## Definition of done

The database decision is ready when we can answer with evidence:

- Is SurrealDB sufficiently mature for a 24/7 Jarvis Home Node?
- Does its Rust SDK fit the Core cleanly?
- Does it materially improve Jarvis memory, graph and AI retrieval?
- Are filtered vector queries correct and performant for our workloads?
- Can it safely store identity, signed approvals, audit and trading records?
- Can we back it up and restore it reliably?
- Can schema changes coexist safely with tag-based Core updates and rollback?
- Is the real UM890 performance acceptable?
- Is one SurrealDB simpler and safer overall than PostgreSQL + pgvector for Jarvis?

Do not begin production migration until those questions are answered and reviewed.
