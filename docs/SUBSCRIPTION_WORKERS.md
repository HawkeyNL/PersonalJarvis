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
installed at `/usr/local/bin/claude` and `/usr/local/bin/codex` before linking.
Both can be installed by the owner from Core Admin or the admin CLI (see
[Installing the Claude Code runtime](#installing-the-claude-code-runtime) and
[Installing the Codex CLI runtime](#installing-the-codex-cli-runtime)). No
updater or worker ever downloads a moving CLI version automatically.
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

`codex` is the one ChatGPT login under `jarvis-codex`. The text-only Codex
chat worker uses it today; the coding path shares it once it is re-reviewed and
enabled (see [Codex chat worker](#codex-chat-worker-text-only-off-by-default)).
Earlier releases had a separate `codex-chat` login under `jarvis-codex-chat`;
see [Retired jarvis-codex-chat identity](#retired-jarvis-codex-chat-identity).

`runtime_missing` means the reviewed official binary or dedicated identity is
missing or its protected state layout failed validation. `wrong_auth_mode` means subscription/ChatGPT authentication
could not be proven; an API-key/PAYG login is not treated as subscription.
`host_unsupported` means a fixed host tool the worker runs through
(`systemd-run`, `env`) is not root-controlled: the tool, every symlink on its
chain and every directory on the way must be root-owned and not group/other
writable (distribution symlinks such as Ubuntu's uutils `/usr/bin/env` are
accepted). Connect prints which tool failed.
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
Codex disconnect disables the chat worker socket, stops the chat worker and
disables/stops its broker and App Server before invoking the official logout
command.
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

## Installing the Claude Code runtime

Core Admin → AI Accounts shows the installed Claude Code version, whether it
is inside the reviewed version contract, whether its ownership is safe, the
current stable release and whether a rollback copy exists. **Install / update
runtime** (channel `stable` or `latest`) and **Roll back runtime** send a fixed
typed request through the existing privileged session; the host-local
equivalents are:

```sh
sudo jarvis accounts runtime status claude
sudo jarvis accounts runtime install claude --channel stable   # or --channel latest / --version 2.1.285
sudo jarvis accounts runtime rollback claude
```

The installer only uses Anthropic's official release service
(`https://downloads.claude.ai/claude-code-releases`, HTTPS only, no redirects,
bounded size and time) and fails closed at every step, in a private work
directory inside the root-only `/usr/local/lib/jarvis` (never `$TMPDIR`):

1. Refuse an unsafe destination: `/usr/local/bin/claude` may not be a symlink
   or foreign-owned, and no ancestor of it or of the rollback directory may be
   group/other-writable or non-root-owned.
2. Resolve the channel pointer (or `--version`) to a strict `MAJOR.MINOR.PATCH`
   version and refuse anything outside the reviewed 2.1.248+ (2.1 line)
   contract before any manifest or binary is downloaded.
3. Verify `manifest.json.sig` with `gpgv` against the Anthropic Claude Code
   release key pinned in the Jarvis binary (`deploy/keys/claude-code-release.asc`,
   fingerprint `31DD DE24 DDFA B679 F42D 7BD2 BAA9 29FF 1A7E CACE`; it is never
   fetched at install time). The signed manifest must name the requested
   version.
4. Download the binary for this platform (`linux-x64`, `linux-arm64` or their
   `-musl` variants), bounded by the signed size (at most 400 MiB), and compare
   its SHA-256 with the signed checksum.
5. If the installed binary already matches the signed checksum, stop without
   changes. Otherwise copy the current binary to
   `/usr/local/lib/jarvis/claude.previous` (root-only `0700` directory), stage
   the new one as `root:root 0755` beside the target, fsync it and rename it
   over `/usr/local/bin/claude`. Leftovers of an interrupted install are
   removed by the next install or rollback.

The candidate is not executed during installation. Linux Claude Code binaries
are not individually code-signed, so the signed manifest plus checksum is the
integrity proof, and the version is bound by that signature. The binary first
runs through the existing hardened paths (`accounts status`, Connect and the
worker), always as `jarvis-claude` in a transient sandbox that re-checks the
version contract before any use. Rollback restores, unverified, whatever
binary the last install replaced (which may predate this installer) and
removes the copy; its version is gated like any other before use. Each install and rollback writes fixed
`provider/action/version/outcome` events to `authpriv` syslog.

Every Claude invocation (worker runs and the accounts login/status/logout
services) sets `DISABLE_UPDATES=1` and `DISABLE_AUTOUPDATER=1`, so the CLI
never updates itself into the worker's writable home; only this owner-triggered
installer changes the runtime.

Manual fallback (for example without Core Admin): download the same
`manifest.json`, `manifest.json.sig` and `linux-x64/claude` from
`https://downloads.claude.ai/claude-code-releases/<version>/`, verify the
signature with `gpgv --keyring <dearmored deploy/keys/claude-code-release.asc>`
and the checksum with `sha256sum`, then
`sudo install -o root -g root -m 0755 claude /usr/local/bin/claude`.

## Installing the Codex CLI runtime

Core Admin → AI Accounts shows, on the Codex card, the installed Codex
version, whether its ownership is safe, the latest GitHub release, whether a
rollback copy exists and whether `cosign` is installed. **Install / update
runtime** (latest release or an exact version) and **Roll back runtime** send a
fixed typed request through the existing privileged session; the host-local
equivalents are:

```sh
sudo jarvis accounts runtime status codex
sudo jarvis accounts runtime install codex --channel latest   # or --version 0.160.0
sudo jarvis accounts runtime rollback codex
```

Owner steps:

1. `sudo apt install cosign` (once). Without it the installer refuses to start.
2. Before the first install, check once that this cosign really rejects what
   it must (see [Checking cosign](#checking-cosign-before-the-first-install)).
3. Install the runtime from Core Admin (or the command above).
4. Review that exact version for the chat worker and set
   `JARVIS_CODEX_REVIEWED_VERSION` (see
   [Codex chat worker](#codex-chat-worker-text-only-off-by-default)); the
   installer never changes the reviewed version.
5. `sudo jarvis accounts connect codex` (the one Codex login), or Connect in
   Core Admin.

The installer only uses `https://api.github.com/repos/openai/codex/releases`
and `https://github.com/openai/codex/releases/download` (HTTPS only, bounded
size and time) and fails closed at every step, in a private work directory
inside the root-only `/usr/local/lib/jarvis` (never `$TMPDIR`):

1. Refuse an unsafe destination (as for Claude: `/usr/local/bin/codex` may not
   be a symlink or foreign-owned, no unsafe ancestor) and a missing or
   non-root-controlled `/usr/bin/cosign`.
2. Resolve `--version` (strict `MAJOR.MINOR.PATCH`) or the latest release from
   the API; the release tag must be exactly `rust-v<version>`. There is no
   version floor here: the chat worker's owner-set reviewed version is the
   gate for use.
3. Download `codex-<arch>-unknown-linux-musl.sigstore` and
   `codex-<arch>-unknown-linux-musl.tar.gz`. GitHub answers with one redirect;
   the installer follows exactly that hop by hand and only to
   `https://objects.githubusercontent.com/` or
   `https://release-assets.githubusercontent.com/`. Each file must match the
   size and SHA-256 `digest` that the GitHub API publishes for it.
4. Decompress with `/usr/bin/gzip` and accept only one ustar/GNU header for the
   regular file `codex-<arch>-unknown-linux-musl` (at most 400 MiB) followed by
   zero padding: links, other or `..`/absolute paths, extension headers and
   extra members are refused.
5. Run, with a clean environment, a private `HOME` and a 3-minute bound:

   ```sh
   cosign verify-blob --offline=true --bundle codex-<arch>-unknown-linux-musl.sigstore \
     --certificate-identity https://github.com/openai/codex/.github/workflows/rust-release.yml@refs/tags/rust-v<version> \
     --certificate-oidc-issuer https://token.actions.githubusercontent.com codex
   ```

   The identity is matched exactly (no regular expression). `--offline` only
   forbids an online Rekor search; the bundle's signed entry timestamp is still
   checked against the Rekor key in Sigstore's TUF trusted root, which cosign
   refreshes from its embedded root.
6. If the installed binary is already identical, stop without changes.
   Otherwise keep it as `/usr/local/lib/jarvis/codex.previous` and atomically
   install the verified binary as `root:root 0755`, exactly as for Claude.

The candidate is never executed as root: `runtime status` runs
`codex --version` only as `jarvis-codex` in the hardened transient service.
Rollback restores, unverified, whatever binary the last install replaced.
Each install and rollback writes fixed `provider/action/version/outcome`
events to `authpriv` syslog.

cosign itself runs as root (it reads the root-only work directory) with no
other input than the two files and the fixed arguments above.

Not verified on the Home Node yet: a real install with the Ubuntu cosign 2.6.2
package (the tests use a fake cosign), including how it fetches the TUF
trusted root.

### Checking cosign before the first install

Run this once as your normal user (no `sudo`) in an empty directory. It
downloads the published 0.160.0 x86_64 release (about 110 MB) and must print
`ok` three times; any `UNEXPECTED` means do not use the installer.

```sh
base=https://github.com/openai/codex/releases/download/rust-v0.160.0
asset=codex-x86_64-unknown-linux-musl
curl -fsSLO "$base/$asset.tar.gz" && curl -fsSLO "$base/$asset.sigstore"
tar -xzf "$asset.tar.gz" "$asset"
sha256sum "$asset"   # 12eb3e81114588aca3b7998f4f19e8997b056aca08e57a7ca7c8a3ec8c652aad
id=https://github.com/openai/codex/.github/workflows/rust-release.yml@refs/tags/rust-v
verify() { cosign verify-blob --offline=true --bundle "$asset.sigstore" \
    --certificate-identity "$1" \
    --certificate-oidc-issuer https://token.actions.githubusercontent.com "$2"; }
verify "${id}0.160.0" "$asset" && echo ok || echo UNEXPECTED
cp "$asset" tampered && printf x >> tampered
verify "${id}0.160.0" tampered && echo UNEXPECTED || echo ok
verify "${id}0.159.0" "$asset" && echo UNEXPECTED || echo ok
```

## Production activation

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

**Identity.** The worker runs as `jarvis-codex` with its ChatGPT login home
`/var/lib/jarvis-codex` (`StateDirectory=jarvis-codex`), the one Codex login
that `sudo jarvis accounts connect codex` links with `codex login
--device-auth`. The login home is the unit's only writable state, so the CLI
can refresh its token. The coding broker, App Server, engineering and
repository state (`/var/lib/jarvis-codex-broker`, `/run/jarvis-codex` with the
App Server socket, `/run/jarvis-codex-broker`, `/var/lib/jarvis-engineering`,
`/var/lib/jarvis-codex-repositories`), the App Server's session history inside
the login home (`.codex/sessions`, `.codex/archived_sessions`,
`.codex/history.jsonl`; only paths that exist when the unit starts can be
hidden), Core's state, the Claude worker and the retired
`/var/lib/jarvis-codex-chat` are inaccessible to it. `--ephemeral` keeps chat
runs out of the session history, and `--ignore-user-config` ignores the shared
`config.toml`. The worker sets `PR_SET_DUMPABLE` to 0 at start, so it writes no
core dump and processes of the same UID cannot ptrace it or read its memory.
`PrivatePIDs` and `ProtectProc=invisible` keep the App Server and broker
processes of the same UID out of its view. `codex` disconnect disables the chat
socket and stops the worker before it logs out.

**Hard requirement before the coding path is enabled.** The text-only chat
worker and the coding path share the one `jarvis-codex` ChatGPT login. Before
the Codex broker or App Server is ever enabled, this shared-token design must
be re-reviewed, covering at least:

- a single owner of the token and its refresh (the chat worker and the App
  Server could otherwise refresh `auth.json` concurrently and invalidate each
  other), refresh races and lock-out on the Home Node;
- the reach of a prompt-injected chat run into coding sessions and their
  history (same UID, same login home);
- config poisoning through the shared, writable login home: a chat run that
  writes `~/.codex/config.toml`, `AGENTS.md`, `rules/`, `skills/`, MCP server
  or `notify` commands would change what the coding path loads and executes;
- same-UID abstract Unix sockets, which file permissions and
  `InaccessiblePaths` do not cover.

Until that review is done and recorded, the coding path stays off. The units
enforce it: `jarvis-codex-chat.socket` and `.service` declare
`Conflicts=jarvis-codex.service jarvis-codex-broker.service`, so starting one
side stops the other, and `verify-home-node.sh` fails when the chat worker and
a coding unit are enabled or running together.

## Research runs (owner opt-in)

Both workers accept a `research` flag in the request. Core sets it only for an
explicit Research request while the owner's `research_web_search` switch in
`routing.json` is on (see `MODEL_ROUTING_OPERATIONS.md`). The request then
carries only the latest question (at most 2000 characters; the worker accepts
at most 8 KiB) and a fixed research instruction. Without the flag, both
workers run exactly as described above, with every tool off.

**Claude.** A research run changes only the tool flags of the reviewed
invocation:

```text
claude -p --output-format json --model <model> --restricted --bare
  --no-session-persistence --tools WebSearch --disallowedTools mcp__*
  --allowedTools WebSearch --max-turns 8 [--system-prompt-file <private file>]
```

`--tools WebSearch` makes `WebSearch` the only built-in tool (the CLI
reference: `--tools` restricts the built-in tools to the named ones;
`--allowedTools` lets that tool run without a permission prompt, which `-p`
cannot show). `WebSearch` runs on Anthropic's web search backend and returns
titles and URLs; it does not fetch pages (Claude Code tools reference,
"WebSearch tool behavior"). `WebFetch`, which would fetch URLs from this host,
stays unavailable: `--restricted` removes it unless `--tools` names it, and
`--tools` does not. MCP stays denied and `--bare`, `--restricted` and
`--no-session-persistence` stay. The CLI reference names no newer version
requirement for these flags, so the 2.1.248 gate is unchanged; a real research
run has not been accepted on the Home Node yet.

**Codex.** A research run changes only `-c web_search=disabled` into
`-c web_search=live` (Codex config reference: `web_search` is
`disabled | cached | indexed | live`). The search is OpenAI's hosted web search
tool; the CLI fetches nothing itself. The event allowlist then also accepts
`web_search` items (codex-rs `exec` JSONL `item.started|updated|completed` with
`item.type == "web_search"`) whose `action.type` is `search`, `open_page` or
`find_in_page`, or that have no action. Every other tool item (command, MCP,
collab tool, file change, plan update), a web search with an unknown or
`other` action, a web search in an ordinary run, and any unknown event still
stops the whole run with `tool_use_refused`.

**Bounds.** A research run stops after 300 seconds (Core waits 310 seconds);
ordinary runs keep 120 seconds. The output bounds are unchanged (Claude: 256
KiB CLI result; Codex: 512 KiB per event, 4 MiB stream, 128 KiB answer). The
two parallel runs per worker are shared with ordinary chat, so two long
research runs can make chat wait for a free slot.

**Prompt injection.** Search results can contain instructions. The research
instruction tells the model to treat web content as data, and the worker
limits what an injected instruction could do: Claude has no tool except
`WebSearch` (besides the CLI's own `EndConversation`) and at most 8 turns,
and Codex runs that use any other tool are killed and discarded. The run's
context holds only the question, so there is no private data to leak into a
search query. No unit or network change is needed: the search runs on the
provider's side and neither worker fetches web pages itself.

### Retired jarvis-codex-chat identity

Earlier releases ran the chat worker as a separate `jarvis-codex-chat` user
with its own login in `/var/lib/jarvis-codex-chat`. Nothing uses it any more
and nothing removes it automatically. `sudo jarvis accounts status codex`,
Core Admin and `verify-home-node.sh` report it as legacy while it exists. After
updating, connect the one Codex login (`sudo jarvis accounts connect codex`),
then remove the old identity and its login home:

```sh
sudo userdel jarvis-codex-chat
sudo rm -rf --one-file-system /var/lib/jarvis-codex-chat
```

This deletes the old login token from the host only. To also end that session
at OpenAI, sign it out in your ChatGPT account's security settings.

**Unit hardening.** On top of every directive of the Claude worker and
`PrivateDevices`, the unit sets `PrivatePIDs`, `ProcSubset=pid`,
`ProtectKernelLogs`, `ProtectClock`, `ProtectHostname`, `RestrictRealtime`,
`RestrictNamespaces`, `SystemCallArchitectures=native`,
`SystemCallFilter=@system-service` and `MemoryDenyWriteExecute`.
`NoExecPaths=/` with `ExecPaths=/opt/jarvis/releases /usr/local/bin/codex
/usr/lib/x86_64-linux-gnu -/usr/lib64` lets only the worker release, the
reviewed CLI and their shared libraries (with the dynamic loader) execute.
`/usr/lib` itself is not listed: it holds shells such as the initramfs busybox
and klibc `sh`. `IPAddressDeny` blocks loopback, link-local,
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

1. Install the official Codex CLI at `/usr/local/bin/codex` with the verified
   installer ([Installing the Codex CLI runtime](#installing-the-codex-cli-runtime)).
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
3. `sudo jarvis accounts connect codex` (or Connect on the "Codex" card in
   Core Admin → AI Accounts), then `sudo jarvis accounts status codex` must
   report `connected`, not `wrong_auth_mode`. This is the same login the
   coding path will use; it stays off (see the hard requirement above).
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
effect of `model_instructions_file` on Codex's built-in instructions and to
whether `--ignore-user-config` also skips a global `AGENTS.md` in the login
home. `codex login status` prints its status line to stderr, possibly after
`WARNING:` lines, and exits 1 when logged out (verified with codex-cli
0.160.0); the worker and the accounts CLI read both streams line by line.
