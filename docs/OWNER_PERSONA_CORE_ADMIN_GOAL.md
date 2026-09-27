# Goal: owner-only Jarvis persona management in Core Admin

Add a safe owner workflow for updating the protected private `Jarvis.md` from the Jarvis Core Admin experience without creating a Jarvis self-modification capability.

Work against CURRENT `main` and preserve:

- `docs/PRIVATE_AGENT_RUNTIME_GOAL.md`;
- `deploy/private/install-private-config.sh`;
- protected `/etc/jarvis/Jarvis.md` ownership/mode/history behavior;
- the existing Core Admin Tauri/polkit/root-broker security boundary.

## Product rule

The owner may update or roll back the Jarvis persona.

Jarvis Core, chat, agents, MCP servers, model output, Codex/Claude workers and normal autonomous updaters may **not** modify the persona.

This is owner administration, not self-modification.

## Safe UX

Add a Core Admin `Persona` / `Jarvis` administration view or a focused section under System.

Show only safe metadata by default:

- installed persona state;
- installed SHA-256 short hash;
- candidate private-checkout hash if available;
- last install time/status;
- whether restart/reload is needed;
- bounded rollback history identifiers.

Actions:

```text
Open/Edit source
Validate candidate
Install candidate
Rollback to previous
Restart Core when required
```

### Editing

Prefer a trusted local owner editing path rather than giving the webview arbitrary filesystem write access.

A safe first implementation may open the canonical private source file in an allowlisted trusted local editor/terminal under the normal desktop user. The frontend must not receive an arbitrary path or a generic shell command.

Do not add a Jarvis API endpoint that accepts persona Markdown from chat or remote clients.

### Installation

Reuse/refactor the existing protected persona installer. The graphical app should call a fixed typed owner-admin operation through the current polkit/root broker.

The privileged helper must resolve only the configured/allowlisted PersonalJarvisAgents checkout and the exact canonical source:

```text
personaljarvis/jarvis-core/Jarvis.md
```

Do not accept an arbitrary root-readable source path from frontend input.

Preserve:

- regular-file/no-symlink checks;
- non-empty validation;
- deterministic hash;
- root-controlled history;
- atomic stage + move;
- `root:jarvis 0640`;
- no raw persona content in logs.

## Validation

Before install, validate at minimum:

- canonical file exists and is regular/non-symlink;
- non-empty;
- bounded maximum size;
- UTF-8;
- no NUL/control-file corruption;
- candidate hash differs or report unchanged;
- optional structural checks for required Jarvis heading/identity markers, without trying to judge model semantics.

Do not let an LLM be the security validator for persona installation.

## Reload/restart

The current Core loads the persona at startup. Until a separately reviewed hot-reload mechanism exists, a successful install should perform or offer a controlled Core restart through the trusted admin path, then verify `/readyz`.

If restart/readiness fails, report clearly. Do not silently overwrite the source checkout or fabricate success.

## Rollback

Expose bounded root-owned persona history using hash identifiers only.

Rollback must:

1. select only a validated history entry;
2. atomically restore it to `/etc/jarvis/Jarvis.md`;
3. preserve ownership/mode;
4. restart/reload Core as required;
5. verify readiness;
6. audit non-secret old/new hashes.

## Hard separation from Jarvis runtime

No runtime capability equivalent to these may exist:

```text
persona.write
persona.install
persona.rollback
persona.edit
```

for agents, MCP tools or conversational Jarvis.

The normal Jarvis app may at most display a read-only persona/config version if useful.

## Core Admin security

Keep the established rules:

- Core Admin itself runs unprivileged;
- no sudo password enters Vue/Tauri;
- no generic shell/filesystem/process IPC;
- privileged mutation through fixed typed broker operations;
- frontend confirmation is not authorization;
- no persona contents in logs, command arguments or captured privileged output.

## Tests

Add coverage for:

1. Jarvis/agents cannot mutate `Jarvis.md`.
2. normal API exposes no persona-write endpoint.
3. Core Admin action is typed and owner/polkit protected.
4. arbitrary source path is rejected.
5. symlink/source traversal is rejected.
6. empty/oversized/invalid candidate is rejected.
7. install is atomic.
8. installed file is `root:jarvis 0640`.
9. unchanged candidate is a no-op.
10. history is root-controlled and bounded.
11. rollback only accepts known history hashes.
12. raw persona content never enters logs/JSON status.
13. Core restart/readiness is checked after activation.
14. failure leaves a recoverable known-good persona.
15. existing protected-path agent/Codex tests remain green.

## Definition of done

The owner can deliberately maintain Jarvis' private persona through Core Admin, while Jarvis itself remains structurally incapable of editing, installing, rolling back or otherwise changing its own governing persona.
