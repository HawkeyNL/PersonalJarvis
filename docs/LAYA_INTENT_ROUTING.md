# Local-first System-1 routing (Laya + Jev)

The Core-owned `FastIntentProvider` contract returns one of five advisory
`WorkKind` labels. `IntentRouterChain` chooses which classifier to consult;
neither classifier chooses a model, executes a tool, grants a capability, or
approves an action. `jarvis-policy`, the owner model allowlist and signed
action-approval paths remain authoritative. The final answer still comes from
one normal Jarvis LLM inference. Classification receives only the latest
bounded user turn, not the persona, hidden reasoning, credentials, agent
prompts or entire conversation.

`JARVIS_LAYA_MODE=off` is the default and preserves Jev-only behavior. In
`shadow`, Jev determines the actual route and local Laya is observed only for
comparison. The authoritative route does not await Laya: at most four shadow
observations run concurrently, each bounded to five seconds, and excess
observations are skipped. A Laya result cannot trigger research, coding or any action. In
`primary`, a valid Laya answer at or above the local threshold determines the
advisory label; otherwise Jev is attempted once if configured and budgeted;
otherwise ordinary Jarvis Auto routing remains. Explicit Fast/Deep/Research
mode and an allowed owner-pinned brain bypass both classifiers. No classifier
failure makes normal chat fail.

Jev uses its existing remote, budget-reserved usage path and its `confidence`
field; its default threshold is `0.75`. Laya runs locally, creates no external
API charge or monetary reservation, and uses `answer_confidence` rather than
Laya's entropy-style `confidence` field. Its default threshold is `0.95`.
These are conservative starting gates, **not** evidence of calibration for
Jarvis intents. Both thresholds and the bounded Laya timeout are trusted Core
configuration (`JARVIS_JEV_CONFIDENCE_THRESHOLD`,
`JARVIS_LAYA_CONFIDENCE_THRESHOLD`, `JARVIS_LAYA_TIMEOUT_MS`, 100–5000 ms).
Keep Laya in shadow until Dutch/English held-out results justify primary.

## Pinned runtime and security boundary

The release packages `jarvis-laya.service`, `jarvis-laya.socket`, `laya-offline.py` and the explicit
`provision-laya` installer under the `laya_runtime: 1` release capability.
The unit is **not enabled automatically**. The Python package is pinned to
[`laya` 0.3.20](https://pypi.org/project/laya/0.3.20/) (reviewed wheel SHA-256
`6039e802fa5effb8dd492061cd7ad39a43087beadc4a4fa4a649614e77eb83d4`;
[upstream source revision](https://github.com/NandhaKishorM/laya/commit/23a17522aa4942da6cce53a995a275760320b691)).
The model bundle is pinned to Hugging Face repository
`convaiinnovations/laya` revision
`55cf4c4ebb4ebe31b2550e8bdf3bd21b99753851`. This revision contains both
the English root checkpoint and a `multilingual/` subdirectory. Stage only
these two reviewed checkpoints; both are preloaded. The specialized typed-decisions
checkpoint is intentionally excluded; its training workflows are not Jarvis
intent data. The wrapper defaults uncertain Latin-language input to the
multilingual checkpoint and only auto-selects English when Laya identifies it.

The package's transitive requirements are not fully pinned by upstream.
Before provisioning, the owner must create and review a complete hash-pinned
`requirements.lock` and matching offline wheelhouse for this Ubuntu/Python
platform; do not install `latest`. Place those root-owned artifacts at:

```text
/var/cache/jarvis-laya/staging/requirements.lock
/var/cache/jarvis-laya/staging/wheels/
/var/cache/jarvis-laya/staging/models/55cf4c4ebb4ebe31b2550e8bdf3bd21b99753851/
/var/cache/jarvis-laya/staging/models.sha256
```

Obtain the English root and multilingual subdirectory from the exact bundle
revision above and allow only
configuration/tokenizer text or JSON and `.safetensors` files. Review the
generated checksum manifest before moving the snapshot into root-owned
staging. No Python model repository code, pickle `.bin` file, symlink, or
world-writable artifact is accepted by the provisioner. Reviewed staging stays
root:root and unchanged across retries; a temporary root:jarvis-laya read-only
wheelhouse/lockfile copy is deleted on success or failure.
The reviewed lockfile accepts only ordinary `package==version` pins with
SHA-256 hashes; index options, remote/direct references and local file URLs
are rejected. Every staged package must be a regular `.whl` whose digest is
listed in that lockfile; source distributions and symlinks are rejected.
The installer runs as `jarvis-laya` inside a separate Linux network namespace
with a controlled environment, using `pip --no-index --require-hashes
--only-binary=:all:`. If namespace creation fails, provisioning stops before
pip runs. This is independent of firewall and proxy settings. Normal service startup has
`HF_HUB_OFFLINE=1` and `TRANSFORMERS_OFFLINE=1` and needs no network.

After a verified Core release contains these artifacts, the owner can run:

```bash
sudo /opt/jarvis/current/provision-laya
sudo systemctl enable --now jarvis-laya.service
curl --fail --silent --unix-socket /run/jarvis-laya.sock http://jarvis-laya.local/health
```

The provisioner requires pre-staged reviewed bytes and does **not** fetch
anything itself. It creates `jarvis-laya` as a separate unprivileged identity,
uses a root-managed runtime and model snapshot, and activates the Python venv
with a local symlink. The service receives only the root-owned systemd Unix
socket `/run/jarvis-laya.sock` (root:jarvis, 0660), never a TCP listener, Caddy
or LAN. Core uses a fixed virtual HTTP hostname over that socket; DNS is not
consulted. systemd passes the listening fd to the unprivileged service, so
`jarvis-laya` does not need membership in the socket's `jarvis` client group.
Core updates capture Laya's active and enabled states separately. An inactive
socket and service remain inactive (even if enabled); a socket-only runtime
remains socket-only; a warm service is deliberately restarted after its socket
and is active with resident models again when activation completes. Failed
Core activation restores the prior units and this same active state. The
updater never enables or disables either unit and does not promise zero
downtime. The service runs CPU-only with a resident model, bounds memory/tasks, and has no
need for `/etc/jarvis/secrets`, private agents, Docker or owner home. The
optional `/etc/jarvis/laya.env` is for trusted local tuning such as
`LAYA_THREADS=2`; the wrapper ignores attempts to set a TCP host or API key.
The root-owned `/run` directory prevents an unprivileged listener from
impersonating a stopped Laya service or intercepting user turns. Only the
trusted `jarvis` group and root can connect; Core still treats all returned
labels as untrusted advice. Laya requests omit the optional upstream `model`
field so Laya's language resolver can select English or multilingual; sending
the alias `laya` would force English and is forbidden here.

## Benchmark and promotion

The public synthetic fixture in `tools/laya/intent-corpus.json` includes Dutch,
English, mixed language, typos/STT-like text, coding, research, actions,
trading-related questions and prompt injection. It contains no owner-private
conversations. Run the local-weight benchmark only after provisioning:

```bash
python3 tools/laya/benchmark.py \
  --pid "$(systemctl show -p MainPID --value jarvis-laya.service)"
```

The report includes raw accuracy (correct / all fixtures), per-class counts
(including unavailable/error fixtures in the denominator), confusion matrix,
threshold coverage (accepted / all fixtures), accepted accuracy (correct
accepted / accepted, or null if none), fallback rate, p50/p95 latency, p99 for at least 100 samples,
requests/s, RSS and process CPU. Cold model load must be measured separately
when starting the service and supplied with `--cold-load-ms`; it is never
fabricated from a warm request. `jev_comparison.available=false` makes clear
that this local-only harness does not compare Jev. The harness never
calls paid Jev directly or bypasses Core's budget. Public upstream benchmark
scores are not Jarvis acceptance evidence; upstream also cautions that base
checkpoints can be overconfident or weak on unfamiliar typed decisions.

For an accepted shadow trial, set `JARVIS_LAYA_MODE=shadow` through trusted
root-managed Core configuration and restart Core. Review safe structured
comparison logs and benchmark accuracy/coverage before explicitly setting
`JARVIS_LAYA_MODE=primary`. Do not enable primary on an unmeasured Home Node.
`/readyz` remains independent of this optional classifier; a stopped service
degrades to Jev or normal Auto routing. Rollback to a pre-Laya Core release
requires the owner to stop and disable `jarvis-laya.service` and
`jarvis-laya.socket` first; the updater
refuses to remove a running/enabled service definition. Rollback then removes
the release-owned unit, while the separately provisioned weights/venv remain
inert for owner-managed cleanup.
