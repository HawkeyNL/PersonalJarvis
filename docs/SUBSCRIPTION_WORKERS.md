# Owner-linked subscription workers

Core Admin → AI Accounts is the sole graphical account-management surface.
`sudo jarvis accounts ...` is the host-local recovery path. The normal Jarvis
app, Core HTTP routes, agents, MCP servers and sandboxes cannot call account
mutation operations. Account status returned to Core Admin is a fixed set of
non-secret fields; provider CLI output is parsed and discarded inside the root
administrator process.

The official clients own authentication. Claude Code uses
`claude auth login`, `claude auth status` and `claude auth logout` under the
dedicated `jarvis-claude` identity. Codex uses `codex login --device-auth`,
`codex login status` and `codex logout` under `jarvis-codex`. These are distinct
from Anthropic/OpenAI API keys. The root-owned official CLI executables must be
reviewed and installed at `/usr/local/bin/claude` and `/usr/local/bin/codex`
before linking; no updater downloads a moving CLI version automatically.
Neither worker uses the owner's home or browser profile. An administrator must
verify the installed CLI version and its supported login/runtime flags against
the [Claude Code authentication documentation](https://code.claude.com/docs/en/authentication),
[Claude Code CLI reference](https://code.claude.com/docs/en/cli-reference) and
[Codex authentication documentation](https://learn.chatgpt.com/docs/auth)
before the first production connection. The current development host has no
installed Claude CLI, so a real subscription-backed run has not been accepted
here.

The reviewed Claude worker invocation requires Claude Code 2.1.248 or newer
within the 2.1 line because `--restricted` was introduced there. Core Admin
checks bounded `--version` output before account linking or declaring the
account connected. The worker repeats that check before a run and returns
`incompatible_runtime` without a model call for an older, unparseable or
unreviewed version. This is a version/flag contract gate, not live billing
proof; a future CLI line needs review. The official CLI reference warns that
`--help` does not list every flag, so it is not used for this gate.

The accounts CLI starts each official login/status/logout command in a bounded
transient systemd service under the appropriate dedicated system user. The
transient service uses `/usr/bin/env -i` before invoking the provider CLI, so
even system-manager default variables do not enter that client. It has a
controlled environment, protected home and filesystem,
and only that worker's private `/var/lib/jarvis-*` state. A session token is
never an argument, API response, database field or Tauri event. The official
provider flow may display an authorization URL or code in the trusted terminal;
do not paste provider access or refresh tokens into Jarvis.
Connect, test, reconnect and disconnect write fixed provider/action/outcome
audit events via local `authpriv` syslog; neither provider output nor the
authorization URL is part of an audit record. If the local audit sink cannot
accept an initiation event, the operation fails before invoking the official
client.

Claude's worker receives only a bounded recent conversation and system text
over `/run/jarvis-claude.sock`. The root-owned socket grants access only to the
`jarvis` group, and the worker checks the peer UID. The worker executes the
official CLI as `jarvis-claude` with a clean environment, no inherited
`ANTHROPIC_API_KEY`, restricted mode, built-in and MCP tools denied, no session persistence and no access to Core's
provider secrets. A system prompt is placed in a private temporary file, not
in process arguments. The worker returns only a typed answer, token counts or
a safe unavailable/plan-limit state. Claude runs are classified as
`subscription`, not metered API calls; explicit `claude-cli` selection cannot
silently fall back to paid Anthropic API credit. The normal Auto router may
consider a *separately owner-enabled* metered API route under its existing
policy, which is not a subscription entitlement.
An unrecognized backend is not classified as local or subscription: it is
unavailable to the router, and pricing diagnostics use an unknown,
conservative estimate rather than zero cost.

Core Admin deliberately reports Claude billing as `overage_unverified` even
when `claude auth status` confirms a subscription login. The CLI cannot verify
the separate account-level extra-usage setting. Linking the account is not a
guarantee that Anthropic cannot bill overage; the owner must disable extra
usage or cap it at zero before explicitly activating the worker socket.

The Codex broker remains deliberately fail-closed: its current
OpenSandbox/task-scoped credential gate has not yet been satisfied. Linking a
ChatGPT/Codex account does **not** enable host Codex execution or mount the
persistent Codex credential store into a coding sandbox. See
`CODEX_OPENSANDBOX_CONTRACT.md`. There is no unsafe host fallback.

Owner commands (no credential values in arguments):

```sh
sudo jarvis accounts list
sudo jarvis accounts status claude
sudo jarvis accounts connect claude
sudo jarvis accounts test claude
sudo jarvis accounts disconnect claude
sudo jarvis accounts status codex
sudo jarvis accounts connect codex
sudo jarvis accounts test codex
sudo jarvis accounts disconnect codex
sudo jarvis accounts status codex-chat
sudo jarvis accounts connect codex-chat
sudo jarvis accounts test codex-chat
sudo jarvis accounts disconnect codex-chat
```

`codex-chat` is the separate ChatGPT login of the text-only Codex chat worker
(identity `jarvis-codex-chat`, see below); `codex` is the coding login.

`runtime_missing` means the reviewed official binary or dedicated identity is
missing or its protected state layout failed validation. `wrong_auth_mode` means subscription/ChatGPT authentication
could not be proven; an API-key/PAYG login is not treated as subscription.
`incompatible_runtime` means the installed Claude CLI is outside the reviewed
version/flag contract. Recheck it after an owner-reviewed CLI update before
enabling the worker socket.
`logged_out` needs an explicit owner-initiated Connect or Reconnect. A plan
limit is reported as a bounded runtime failure and never authorizes paid
credits. Jarvis never reconnects accounts itself.

The release capability `tooling.subscription_workers = 1` binds the Claude
worker executable and its two systemd units to the release checksums. Older
verified releases lacking the capability remain valid; no worker files are
invented for them. The Claude socket is not enabled by routine Core update or
Connect. Anthropic's account-level extra-usage/usage-credit setting can cause
additional billing beyond a subscription, and the documented CLI status does
not prove that it is disabled. Therefore production worker activation remains
an explicit owner-operated acceptance step after verifying that setting (or a
provider-documented machine-readable no-overage control is available).
Disconnect disables the socket and stops active Claude runs before logout.
Codex disconnect disables/stops its broker and App Server before invoking the
official logout command.
Neither worker owns a TCP listener.

After a reviewed release is installed, the official CLI is verified and the
owner has disabled extra usage or set its limit to zero in the Anthropic
account, the owner can deliberately enable the local worker with
`sudo systemctl enable --now jarvis-claude.socket`. The socket is never
enabled by `accounts connect` or by a Core update. `sudo jarvis accounts
status claude` reports login status, not a verified billing ceiling.
Core uses a `claude-cli` model only when that exact pair is enabled in
`policy.json`. `sudo jarvis models refresh claude-cli` records the configured
Claude tier models (disabled); `sudo jarvis models register claude-cli <model>`
records any other one. Then `sudo jarvis models enable claude-cli <model>`.

PR #58 delivers account linking, the isolated Claude worker, compute-class
accounting and bounded delegated-context primitives. It does not enable Codex
coding execution. A separate execution PR must implement the reviewed
OpenSandbox workload, task-scoped proxy, repository snapshot authority and run
lifecycle before that path can be activated. This missing execution layer does
not block merging the fail-closed foundation.

Before production activation, the owner must install and review the official
CLIs at their pinned host paths, verify/disable Claude extra usage in the
Anthropic account, connect and test each account through Core Admin or the
local admin CLI, and explicitly enable the Claude worker socket. A harmless
bounded Claude prompt should then confirm worker routing and subscription
telemetry with no metered-API entry. Real account login and Home Node behavior
have not been established by mock/fixture tests. The private `Jarvis.md`
persona remains owner-managed and is not changed by this foundation.

## Codex chat worker (text only, off by default)

The `codex-cli` provider lets the owner route chat to Codex models through
the ChatGPT subscription, for example GPT-6 Luna. It is a chat brain only: it
has no tools and is unrelated to Codex coding. It does not use, change or
weaken the fail-closed Codex coding broker (`CODEX_OPENSANDBOX_CONTRACT.md`).

Core sends a bounded recent conversation and system text over
`/run/jarvis-codex-chat.sock` (root:jarvis 0660, socket-activated). The
`jarvis-codex-chat-worker` checks that the peer is the `jarvis` user, allows
two parallel runs, and before each run checks the reviewed CLI version and
that `codex login status` reports a ChatGPT login. An API-key login is never
treated as a subscription. Each run gets a private 0700 directory under
`/run/jarvis-codex-chat` with an empty working directory. The system prompt
goes into a 0600 file there (`model_instructions_file`), never into the
arguments. The prompt is written to stdin. The CLI starts from a cleared
environment. The worker refuses to start if any `*_API_KEY` variable is set,
and the unit unsets `OPENAI_API_KEY` and `CODEX_API_KEY`. The exact
invocation is:

```text
codex exec --json --ephemeral --skip-git-repo-check --ignore-user-config --ignore-rules
  --sandbox read-only -m <model>
  -c features.shell_tool=false -c features.unified_exec=false
  -c web_search=disabled -c tools.view_image=false -c features.apps=false
  -c features.multi_agent=false -c features.memories=false -c features.hooks=false
  -c history.persistence=none -c analytics.enabled=false -c approval_policy=never
  -c shell_environment_policy.inherit=none -c forced_login_method="chatgpt"
  [-c model_instructions_file="<private 0600 file>"] -
```

Codex ignores an unknown `-c` key, so a renamed key could silently turn a
tool back on. The worker therefore does not trust the `-c` settings alone. It
reads the `--json` event stream line by line and allows only thread and turn
lifecycle events, reasoning, agent messages and error events. Any other event
or item type (a command, web search, MCP or tool call, file change, plan
update), an unknown type or a line that is not JSON stops the run at once:
the worker kills the CLI's whole process group, discards the answer and
returns the fixed state `tool_use_refused`. Core treats that as a failed
attempt of this entry. The answer is the last completed agent message, used
only when the turn completed and the CLI exited successfully.

The CLI runs in its own process group. A timeout or refusal kills the whole
group. A run stops after 120 seconds. An event line is at most 512 KiB, the
event stream at most 4 MiB and the answer at most 128 KiB. The CLI's
diagnostics are kept to 16 KiB, used only to classify a failure, and then
discarded. A failure is classified only from structured error events and
diagnostic lines that start with `ERROR:`, never from model text, so a model
answer cannot pose as a plan limit or a missing model. The worker returns a
typed answer or one fixed state. `model_unavailable` covers the ChatGPT
rejection "The '<model>' model is not supported when using Codex with a
ChatGPT account" (openai/codex#47784, a staged Luna rollout). It is final for
that model. The router then tries only the owner's next chain entry and never
falls back to the paid OpenAI API. `codex-cli` is a subscription backend, not
metered. `paid_api: "off"` keeps it, and a metered entry after it still needs
`metered_after_subscription`. It is in no built-in order: Core reaches it only
through an owner-routed chain or a brain pin, always with an exact model that
is enabled in `policy.json`.

**Identity.** The worker runs as its own system user `jarvis-codex-chat`
with its own ChatGPT login home `/var/lib/jarvis-codex-chat`
(`StateDirectory=jarvis-codex-chat`). The owner links it separately with
`sudo jarvis accounts connect codex-chat`, which runs
`codex login --device-auth` under that identity. It shares no UID, login or
state with the `jarvis-codex` identity of the coding broker and App Server, so
a compromised chat CLI cannot signal those processes or read their login. The
prepare and install scripts create the identity like the other worker
identities (nologin shell, own group, no Docker group, private 0700 home). The
unit makes the login home its only writable state. The `jarvis-codex` login
home, the coding broker, App Server, engineering and repository state
(`/var/lib/jarvis-codex`, `/var/lib/jarvis-codex-broker`, `/run/jarvis-codex`,
`/run/jarvis-codex-broker`, `/var/lib/jarvis-engineering`,
`/var/lib/jarvis-codex-repositories`), Core's state and the Claude worker are
inaccessible to it. The worker sets `PR_SET_DUMPABLE` to 0 at start, so it
writes no core dump and processes of the same UID cannot ptrace it or read its
memory. `PrivatePIDs` gives it its own PID namespace. `codex-chat` disconnect
disables the chat socket and stops the worker before it logs out; `codex`
disconnect no longer touches the chat worker.

**Unit hardening.** On top of every directive of the Claude worker and
`PrivateDevices`, the unit sets `PrivatePIDs`, `ProcSubset=pid`,
`ProtectKernelLogs`, `ProtectClock`, `ProtectHostname`, `RestrictRealtime`,
`RestrictNamespaces`, `SystemCallArchitectures=native`,
`SystemCallFilter=@system-service` and `MemoryDenyWriteExecute`.
`NoExecPaths=/` with `ExecPaths=/opt/jarvis/releases /usr/local/bin/codex
/usr/lib -/usr/lib64` lets only the worker release, the reviewed CLI and their
shared libraries execute. `IPAddressDeny` blocks loopback, link-local,
multicast, the private IPv4 ranges, CGNAT/Tailscale (`100.64.0.0/10`) and ULA
(`fc00::/7`), so the CLI reaches only the internet and cannot reach Core,
Ollama or the LAN. `IPAddressAllow=127.0.0.53` keeps DNS working because the
Home Node resolves through the systemd-resolved stub (`/etc/resolv.conf` links
to `stub-resolv.conf`); a host with another resolver must put its address
there instead. The unit sets no `RemoveIPC`.

These settings must be checked against the reviewed CLI, which has not been
possible here. A native (Rust) Codex binary is expected to work. If the
reviewed CLI needs another interpreter or helper path, the owner adds it to
`ExecPaths`. If it is a JIT runtime (for example a Node.js build),
`MemoryDenyWriteExecute` must be removed. Both fail closed: with a wrong list
the CLI cannot start and every run fails; nothing runs with fewer limits.

**Version gate.** The worker reads `JARVIS_CODEX_REVIEWED_VERSION` once at
start. systemd sets it from the optional, root-owned
`/etc/jarvis/codex-chat-worker.env` (`EnvironmentFile=-` in
`jarvis-codex-chat.service`; systemd reads it before the sandbox applies). Before
each run the worker requires exactly one version number in `codex --version`
output, equal to that value. Unset, empty or malformed (for example `v1.2.3`
or `1.2`) means every run returns `incompatible_runtime` without a model call.
A newer CLI needs a new review and a new value; there is no "or newer". No
Codex CLI version has been verified for this worker yet. Changing the version
needs no code change or release.

Release capability `tooling.codex_chat_worker = 1` binds the worker binary and
both units to the release checksums. Nothing enables the socket: not the
installer, an update or account linking. An update only restarts a worker that
is already running. A rollback to a release without the worker is refused
while the socket or service is enabled or active.

Owner activation:

1. Install the official Codex CLI as a root-owned regular file at
   `/usr/local/bin/codex` (not a symlink or an npm wrapper).
2. Check that this version supports every flag and `-c` key in the invocation
   above (`codex exec --help` and the official non-interactive and config
   documentation), that its `--json` events use the type names the worker
   allows, and that it is a native binary needing nothing outside the unit's
   `ExecPaths` (`file /usr/local/bin/codex`, `ldd`). Then record that exact `codex --version` number, for
   example `1.2.3`:
   `echo 'JARVIS_CODEX_REVIEWED_VERSION=1.2.3' | sudo tee /etc/jarvis/codex-chat-worker.env`
   (root-owned, `0644`; it is not a secret). After a CLI update, review again
   and change the value, then `sudo systemctl try-restart
   jarvis-codex-chat.service`.
3. `sudo jarvis accounts connect codex-chat` (or Connect on the "Codex chat"
   card in Core Admin → AI Accounts), then `sudo jarvis accounts status
   codex-chat` must report `connected`, not `wrong_auth_mode`. This is a
   separate login from `codex`; linking one never links the other.
4. Record the exact pair, then enable it:
   `sudo jarvis models register codex-cli gpt-6-luna` and
   `sudo jarvis models enable codex-cli gpt-6-luna` (or "Register subscription
   model" and Enable in Core Admin's Models view). Subscriptions have no model
   catalog, so `refresh` cannot discover Codex models; `register` adds the pair
   to `policy.json` as discovered and disabled (`source: owner_registered`).
5. `sudo systemctl enable --now jarvis-codex-chat.socket`.
6. Route a tier, for example `sudo jarvis models route set hard codex-cli
   gpt-6-luna claude-cli claude-opus-5`, or use Core Admin. This restarts Core,
   which then sees the socket and marks the brain available.
7. Send one harmless prompt and confirm subscription telemetry with
   `backend: codex-cli` and no metered-API entry. If Luna is not yet available
   to the account, expect a `model_unavailable` error, not a paid call.

Not verified here: a real ChatGPT login, a real `codex exec` run, and that the
installed CLI accepts every flag and key above. The same applies to the exact
`--json` event and item type names the worker allows (taken from the codex-rs
`exec` JSONL events; a renamed type is refused, so a mismatch fails closed),
and to the unit's `ExecPaths`, `MemoryDenyWriteExecute` and DNS settings with
the real CLI. The same applies to the
effect of `model_instructions_file` on Codex's built-in instructions, to
whether `codex login status` prints to stdout (the accounts CLI makes the
same assumption), and to whether `--ignore-user-config` also skips a global
`AGENTS.md` in the login home.
