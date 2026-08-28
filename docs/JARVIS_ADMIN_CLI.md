# Jarvis Home Node administration

The canonical owner interface on a provisioned Home Node is the root-operated,
allowlisted command:

```bash
sudo jarvis status
sudo jarvis health
sudo jarvis update --check
sudo jarvis update
```

`jarvis` is installed as a root-owned executable at `/usr/local/sbin/jarvis` by
the idempotent Home Node preparation/install flow. It does not grant Jarvis
Core, agents, Codex, or OpenSandbox any administrative authority.

## Updates

```bash
sudo jarvis update                 # latest published stable release
sudo jarvis update --version v0.0.9
sudo jarvis update --check          # non-mutating; exit 2 means available
sudo jarvis update --status
sudo jarvis update --rollback       # asks for confirmation
```

Only GitHub Releases that are neither draft nor prerelease are accepted. The
existing verified-release protocol downloads the artifact and checksum over
HTTPS, validates archive layout and release manifest, preserves the previous
release, restarts Core, and waits for bounded readiness checks. A failed
activation restores the previous known-good release. Automatic timer updates
continue to refuse schema-changing releases; perform those manually with a
backup and recovery plan.

## Models and credentials

```bash
sudo jarvis models list
sudo jarvis models status
sudo jarvis models refresh
sudo jarvis models enable openai-api gpt-4o-mini
sudo jarvis credentials list
sudo jarvis credentials set openai
```

The command delegates to the existing root-managed policy and credential
helpers. A provider key never enables a model on its own. Credential input is
accepted only from the controlling TTY and is never accepted in an argument,
printed, or written to the journal.

## Private agents

```bash
sudo jarvis agents status
sudo jarvis agents check
sudo jarvis agents update
sudo jarvis agents rollback
```

The private repository credential remains confined to the root-only private
agent updater. The Core, agent bundle, Codex and OpenSandbox do not receive it.
Agent rollback may only select a validated immutable bundle already under
`/var/lib/jarvis/agents/releases`.

## Diagnostics

```bash
sudo jarvis services status
sudo jarvis logs core --lines 100
sudo jarvis logs updater
sudo jarvis logs agents --follow
```

Log targets are allowlisted; the command is not a generic `journalctl` or
`systemctl` passthrough. For the full command list use `sudo jarvis --help`.
The previous `jarvis-models`, `jarvis-credentials`, and private updater helpers
remain compatibility/internal fallbacks, but normal owner operations should use
`sudo jarvis ...`.
