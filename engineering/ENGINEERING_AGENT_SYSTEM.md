# Engineering Agent System

## Lifecycle

```text
Request
→ Architecture Research
→ Codebase Impact Analysis
→ Design/ADR
→ Security Review
→ Implementation
→ Tests
→ Independent Reviews
→ Fix/Reverification
→ Release Gate
→ Production Observation
→ Improvement Planning
```

## Agents

### Engineering Orchestrator
Routes tasks, checks dependencies, prevents conflicting changes and enforces gates.

### Architecture Research Agent
Investigates existing architecture, official documentation, suitable design patterns, future expansion, scalability, security, reliability, operating cost, migration paths and vendor lock-in. It must compare alternatives.

### Codebase Impact Agent
Maps affected crates, APIs, schemas, permissions, clients, jobs, tests, deployments, backward compatibility and operational blast radius.

### Design Agent
Produces component boundaries, contracts, state machines, failure behaviour, extension points, ADR, test strategy, rollout and rollback plans.

### Coding Agents
Implement only after required design gates are passed.

### Independent Reviewer Agents
Architecture, security, correctness, performance, database, API contract, UX/accessibility, tests and operations. The implementer cannot approve its own work.

### Fix Agent
Reproduces findings, identifies root cause, implements the smallest safe fix, adds regression tests and requests re-review.

### Observability Intelligence Agent
Analyzes logs, traces, metrics, errors, slow queries, queue delays, model latency, costs and resource saturation. It creates improvement plans but cannot modify production automatically.

### Incident Learning Agent
Builds incident timelines, identifies root cause/contributing factors, creates remediation tasks and updates runbooks/tests.
