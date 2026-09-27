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
```

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
