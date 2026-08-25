# Goal: private agent runtime boundary and immutable Jarvis persona

Implement a production-safe agent registry/runtime boundary in the public `PersonalJarvis` repository while keeping all real personal agent definitions private in `HawkeyNL/PersonalJarvisAgents`.

## Context

`PersonalJarvis` is public and contains runtime/API/app/deployment code. `PersonalJarvisAgents` is private and contains the real personal agent Markdown definitions. Public GitHub CI MUST NOT require access to the private repository or any private agent content.

The current `jarvis-core` only owns persona loading (`load_persona`) and explicitly says future orchestration has not yet been implemented. There is currently no production Agent Registry/loader/runtime boundary in `jarvis-core`.

The private agent repository explicitly defines its profiles as documentation/contracts: profiles may request capabilities but MUST NOT grant themselves runtime capability, policy exceptions, secret access, or execution rights. The public runtime remains authoritative.

## Required architecture

Implement public runtime mechanics only:

- `AgentDefinition`
- `AgentRegistry`
- `AgentLoader`
- validation/schema/versioning
- immutable deployed agent bundle loading
- agent run/delegation primitives suitable for later model routing/tool execution
- capability intersection with Core/policy authority
- safe reload/version transition
- change-proposal representation for owner-approved changes

Do NOT add the real private agent Markdown files to this repository.

Production conceptual flow:

```text
private PersonalJarvisAgents checkout
        ↓ trusted owner/deployment path only
validate agent definitions
        ↓
create immutable versioned bundle
        ↓
/var/lib/jarvis/agents/releases/<bundle-id>/
        ↓ atomic symlink
/var/lib/jarvis/agents/current
        ↓ READ ONLY
Jarvis Core AgentRegistry
        ↓
validated temporary AgentRun
```

Jarvis Core must not require Git credentials and must not clone/pull `PersonalJarvisAgents` itself.

## Agent definition contract

Define a strict versioned format for Markdown agent definitions, preferably YAML frontmatter plus Markdown instructions, for example conceptually:

```yaml
---
schema_version: 1
id: research
name: Research Agent
model_policy: research
requested_capabilities:
  - web.search
max_parallel_runs: 2
---

# Instructions
...
```

Choose exact fields based on existing architecture.

At minimum validate:

- schema version
- stable safe agent ID
- display name
- non-empty instructions
- requested capabilities
- model/routing policy reference if present
- concurrency/resource hints if supported
- duplicate IDs
- unknown/unsupported schema fields according to a deliberate compatibility policy

Never execute instructions during validation.

## Capability authority

An agent definition may REQUEST capabilities only.

Effective capabilities MUST be computed outside the private profile:

```text
requested by agent
INTERSECT
runtime-supported capabilities
INTERSECT
Core/security policy
INTERSECT
current user/device/session permissions
INTERSECT
task/risk approval state
=
effective capabilities
```

A profile containing `shell.root`, `secrets.read_all`, `ibkr.trade`, or any other capability must gain nothing unless the public runtime/policy independently allows it for that task.

Unknown capabilities fail closed.

## Agent execution model

Do NOT implement every agent as a permanent Linux process.

Model agents as validated definitions used to create bounded `AgentRun`s. An `AgentRun` should have enough state for future orchestration, such as:

- run ID
- agent ID/version
- task
- lifecycle state
- effective capabilities
- selected model/routing policy reference
- budgets/limits
- timestamps
- optional sandbox/tool context references

Do not bypass existing policy, approval, OpenSandbox, Codex, trading, or secret boundaries.

The registry/runtime should be usable later for Core delegation such as research + trading agents in parallel, but this milestone does not need to implement every model provider.

## Private bundle deployment

Add a trusted deployment utility in the public repository that can consume a LOCAL path to an already-authenticated private `PersonalJarvisAgents` checkout.

Example conceptual invocation:

```bash
sudo deploy/systemd/stage-agent-bundle.sh /path/to/PersonalJarvisAgents/agents
```

Requirements:

1. Never fetch GitHub credentials.
2. Never clone the private repository itself.
3. Validate every definition first.
4. Refuse symlinks, path traversal, devices, FIFOs, sockets, unexpected executable content, or files outside the allowed source tree.
5. Calculate deterministic cryptographic hashes.
6. Create a versioned immutable bundle under `/var/lib/jarvis/agents/releases/<bundle-id>` or an equally protected production location.
7. Bundle contents must be owned by a trusted deployment owner and read-only to the `jarvis` runtime user.
8. Atomically switch `current` only after the complete bundle validates.
9. Keep the previous known-good bundle for rollback.
10. Never modify the source private checkout.
11. Do not copy `.git`, secrets, internal documentation outside the intended `agents/` definitions, or arbitrary files from `personaljarvis/`.
12. Produce useful local diagnostics without logging private agent instruction contents.

If validation fails, keep the currently active bundle untouched.

## Runtime loading

Jarvis Core should load only the staged production bundle, not arbitrary Git working trees.

Recommended production configuration:

```text
JARVIS_AGENT_BUNDLE_DIR=/var/lib/jarvis/agents/current
```

Requirements:

- startup validates active bundle metadata/hash/schema
- missing bundle should produce an explicit degraded state; choose whether zero optional agents is allowed, but never silently load arbitrary fallback private content
- registry is immutable/snapshot-based per generation
- reload is atomic
- in-flight runs remain pinned to the definition/version they started with
- new runs use the new generation
- failed reload leaves previous registry active
- expose safe health metadata: bundle ID, generation, number of agents, loaded/degraded state; never expose full private prompts through public health endpoints

## Agent modification policy

Jarvis Core, agents, OpenSandbox workloads, Codex runtime tasks, and normal updater tasks MUST NOT have write access to the deployed agent bundle or the private agent source checkout.

Jarvis may propose a change, but it must be represented as a structured `AgentChangeProposal`/diff and require explicit owner approval before any trusted external development/deployment flow changes the private repository.

Do NOT implement an API endpoint that lets an LLM directly overwrite agent Markdown.

Do NOT give Core a GitHub credential for `PersonalJarvisAgents`.

## Jarvis.md hard invariant

The canonical Jarvis persona is especially protected.

The current public Core already loads a persona from an on-disk path through `load_persona`. Preserve that read-only model and strengthen deployment protections.

The running system MUST NOT have a supported self-modification path for `Jarvis.md`.

At runtime:

- Jarvis Core: read only
- agent runs: read only if needed
- OpenSandbox: no write access
- Codex runtime: no write access
- normal autonomous updater: no content-generation/edit authority

Changing `Jarvis.md` is a software-development action performed through the normal owner-controlled Git branch/PR/release process, not a runtime agent action.

Add tests/guards so protected-path mutation attempts through runtime/Codex/worktree mechanisms fail closed.

Do not rely only on text inside `Jarvis.md` saying it cannot be modified; enforce this structurally through filesystem/deployment/policy boundaries.

## Public CI isolation

The public `PersonalJarvis` GitHub Actions workflows MUST NOT receive credentials for or checkout `PersonalJarvisAgents`.

GitHub's repository `GITHUB_TOKEN` is repository-scoped; preserve that separation. Do not add PATs, personal SSH keys, deploy keys, or GitHub App credentials for the private agent repo to ordinary public CI.

Use public synthetic fixtures instead, for example:

```text
tests/fixtures/agents/
  example-research.md
  example-coding.md
```

Fixtures must contain no real personal prompts/private information.

CI should test parser/validation/registry/bundle behavior entirely from those fixtures.

## Host checkout layout

The two source repositories may be checked out separately on the Home Node, e.g.:

```text
/home/gus-jarvis-home/PersonalJarvis
/home/gus-jarvis-home/PersonalJarvisAgents
```

The production Core must not depend on those exact paths. The private checkout is an owner/deployment input; the staged bundle is the runtime input.

Document the recommended local installation flow without exposing private contents.

## Security tests

Add tests covering at minimum:

1. valid fixture agent loads
2. malformed frontmatter rejected
3. unsupported schema rejected safely
4. duplicate agent IDs rejected
5. empty instructions rejected
6. malicious/path-like IDs rejected
7. unknown capability cannot grant itself permission
8. requested capability is removed by policy intersection
9. immutable registry generation behavior
10. failed reload keeps previous generation
11. in-flight run remains pinned to old generation
12. staging refuses symlinks/path traversal/special files
13. staging copies only intended agent definitions
14. staging does not copy `.git` or private `personaljarvis/` docs
15. active bundle is read-only to runtime user
16. bundle tampering/hash mismatch is detected
17. owner-approved bundle switch is atomic
18. protected `Jarvis.md` cannot be mutated through runtime agent APIs
19. protected `Jarvis.md` cannot be made writable through Codex worktree preparation
20. public CI tests need no private-repo credential

## Deployment integration

Update Home Node deployment docs/scripts to include, in the correct order:

1. clone/update public `PersonalJarvis` as owner/developer
2. clone/update private `PersonalJarvisAgents` separately as owner/developer
3. prepare Home Node
4. stage/validate the private `agents/` bundle using the trusted script
5. install/start Core with read-only bundle path
6. verify Core health reports correct bundle ID/count
7. update private agents by owner-controlled Git workflow, then stage a new bundle
8. rollback to previous bundle if necessary

Do not expose the private repo through Caddy/public API.

## Documentation

Document clearly:

- public repo = runtime/security authority
- private repo = personal agent definitions/internal docs
- agent profiles request but do not grant capabilities
- Core reads a staged immutable bundle
- Core cannot modify private source or deployed definitions
- `Jarvis.md` cannot be self-modified
- public CI has no access to private agent data
- owner-controlled update/rollback procedure

## Definition of done

Complete when:

- public runtime contains a tested AgentDefinition/AgentRegistry/loader boundary
- real private prompts remain absent from public repo and CI
- a local private `agents/` checkout can be validated/staged into a versioned immutable production bundle
- Core can load/reload that bundle atomically
- agent runs are temporary bounded runtime objects rather than permanent daemons by default
- capabilities are intersected with public runtime/policy authority and fail closed
- Core cannot write private agent source or deployed bundles
- agent changes require an explicit owner-controlled proposal/deployment path
- `Jarvis.md` has no supported runtime self-modification path and protected-path tests enforce it
- public CI never requires credentials to `PersonalJarvisAgents`
- Home Node deployment/verification docs cover the complete flow
- all tests/lints/security checks pass

Do not weaken existing approval, sandbox, trading, Codex, secret, or device-security boundaries to implement this.