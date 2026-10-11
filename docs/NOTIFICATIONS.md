# Owner notifications

Core can push a short, generic notification to the owner's phone through a
self-hosted [ntfy](https://docs.ntfy.sh/) server on the Home Node. It is off
until `JARVIS_NOTIFY_NTFY_URL` is set.

## What is sent

| Event | Message | Priority | Tap opens |
|---|---|---|---|
| Monthly LLM spend reaches 90% of the hard budget (`JARVIS_LLM_MONTHLY_BUDGET_EUR`), once per calendar month (UTC) | Budget reached 90% this month | default | `jarvis://health/usage` |
| A new device pairing request was created (always with a verified account password) | New device wants to pair — open Jarvis to review | high | `jarvis://approvals/pairing` |

Every push has the title `Jarvis` and nothing else: no amounts, IP addresses,
device names or key fingerprints. The app shows the details after the owner
signs in, and approval always goes through the app's signed flow, never
through a notification action.

Behaviour in Core:

- Handlers only enqueue (bounded queue of 16; a full queue drops the event
  and logs a count). One worker publishes with a 5 s timeout per request and
  up to 3 attempts with backoff; 4xx answers other than 429 are not retried.
- The same event kind is sent at most once per 60 s.
- The budget marker is kept in memory. At startup a month that is already at
  or above 90% counts as notified, so restarts do not repeat the push.
- Logs name the event kind and HTTP status only, never the token or topic.
- Core only creates a pairing request after verifying the owner's account
  password; an owner without one gets `403` (`pairing_password_not_set`) and
  no push, so an unauthenticated caller cannot trigger one.
- An invalid URL or a missing or malformed token is logged at startup and
  leaves notifications off; Core keeps running.

What leaves the Home Node: without `upstream-base-url`, nothing beyond the
phones' own connections. With it set (needed for the iOS ntfy app), every
published message is announced to ntfy.sh as a poll request carrying the
message id and a hash of the topic URL, whether or not an iPhone is
subscribed; ntfy.sh then sees the Home Node's IP
address and the timing, and Apple sees the timing of the APNs push. The
message text stays on the Home Node. Leave `upstream-base-url` out of
`server.yml` if no iPhone uses the ntfy app.

## 1. Install ntfy (owner)

Install the ntfy server from its official package source
(<https://docs.ntfy.sh/install/>). Do not confuse it with the unrelated
Debian/Ubuntu `ntfy` Python package. The package's default configuration
listens on port 80 without authentication, so stop it before anything else:

```bash
sudo systemctl disable --now ntfy
sudo install -o root -g ntfy -m 0640 deploy/ntfy/server.yml /etc/ntfy/server.yml
sudoedit /etc/ntfy/server.yml          # set base-url to https://ntfy.<domain>
sudo install -D -o root -g root -m 0644 deploy/ntfy/ntfy.service.d-jarvis.conf \
  /etc/systemd/system/ntfy.service.d/jarvis.conf
sudo systemctl daemon-reload
sudo systemctl enable --now ntfy
ss -ltn | grep 2586                     # must show 127.0.0.1:2586 only
```

## 2. Users, topic and token (owner)

Pick a random topic name and keep it with the owner's notes, for example
`jarvis-$(openssl rand -hex 8)`. Run the user commands as the `ntfy` user so
the auth database keeps its owner:

```bash
sudo -u ntfy ntfy user add jarvis-publisher      # prompts for a password
sudo -u ntfy ntfy access jarvis-publisher <topic> write-only
sudo -u ntfy ntfy token add jarvis-publisher     # prints tk_...
sudo -u ntfy ntfy user add owner                 # the phone login, see below
sudo -u ntfy ntfy access owner <topic> read-only
```

The `owner` password is reachable from the internet through Caddy: generate a
strong random one (for example `openssl rand -base64 24`) and keep it only in
a password manager and the phone apps.

The publisher token is a Core secret like the provider credentials: one line
in `/etc/jarvis/secrets/ntfy.env` with the same `root:jarvis 0640`
boundary (checked by `verify-home-node.sh`). The shipped `jarvis-core.service`
loads it through `EnvironmentFile=-/etc/jarvis/secrets/ntfy.env`.

```bash
sudo install -o root -g jarvis -m 0640 /dev/null /etc/jarvis/secrets/ntfy.env
sudoedit /etc/jarvis/secrets/ntfy.env     # one line: JARVIS_NOTIFY_NTFY_TOKEN=tk_...
```

Core accepts the token only as one printable word of at most 256 bytes.

## 3. Public hostname (owner)

Add a DNS record `ntfy.<domain>` for the Home Node, add
`JARVIS_NTFY_HOSTNAME=ntfy.<domain>` to the environment file that Caddy loads
(see `deploy/caddy/caddy.service.d-jarvis.conf`), uncomment the ntfy site block
at the end of `deploy/caddy/Caddyfile` in the installed copy, then
`sudo systemctl reload caddy` (Caddy keeps the old config if the new one is
invalid). No new port: it uses the existing 443.

## 4. Phones

Both apps use the `owner` login, which can only read the topic.

- **Android**: install ntfy (F-Droid or Play). Settings → Manage users → add
  `https://ntfy.<domain>` with the `owner` login. Then subscribe with **+**,
  enter the topic, choose "Use another server" and enter
  `https://ntfy.<domain>`. The app keeps its own connection to the Home Node;
  no Google service is involved for a self-hosted server. Registering the
  Jarvis app itself as a UnifiedPush client is later app work.
- **iOS**: install the ntfy app. Settings → Users → add
  `https://ntfy.<domain>` with the `owner` login, then subscribe to the topic
  on that server. iOS delivery depends on `upstream-base-url` in `server.yml`:
  ntfy.sh only forwards a poll request with the message id through APNs, and
  the app then fetches the message from the Home Node (see "What leaves the
  Home Node" above).
- `enable-login` stays `false` in `server.yml`: it only concerns the ntfy web
  app, which is disabled. Turn it on only if a phone app turns out to need it.

## 5. Enable in Core (owner)

Add to `/etc/jarvis/core.env`:

```bash
JARVIS_NOTIFY_NTFY_URL=http://127.0.0.1:2586/<topic>
```

The URL must be `http` to a loopback address or `https`, with no credentials,
query or fragment, and the path must be exactly one topic name
(`[A-Za-z0-9_-]`, up to 64 characters). Then:

```bash
sudo systemctl restart jarvis-core
journalctl -u jarvis-core --since -5min | grep 'owner notifications'
```

Expect `owner notifications enabled`. Test by starting a pairing request from
a new device: the phone shows "New device wants to pair".

## Rollback

Remove `JARVIS_NOTIFY_NTFY_URL` from `/etc/jarvis/core.env` and
`sudo systemctl restart jarvis-core`. To remove ntfy as well:
`sudo systemctl disable --now ntfy`, comment the ntfy block in the installed
Caddyfile again and `sudo systemctl reload caddy`, then delete
`/etc/jarvis/secrets/ntfy.env` and restart Core again.
