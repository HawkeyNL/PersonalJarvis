# Owner notifications

Core can push a short, generic notification to the owner's phone through a
self-hosted [ntfy](https://docs.ntfy.sh/) server on the Home Node. It is off
until `JARVIS_NOTIFY_NTFY_URL` is set.

## What is sent

| Event | Message | Priority | Tap opens |
|---|---|---|---|
| Monthly LLM spend reaches 90% of the hard budget (`JARVIS_LLM_MONTHLY_BUDGET_EUR`), once per calendar month (UTC) | Budget reached 90% this month | default | `jarvis://health/usage` |
| A new device pairing request was created with a verified account password | New device wants to pair — open Jarvis to review | high | `jarvis://approvals/pairing` |
| A signed-in device asks another device to approve its unlock | Approval waiting — open Jarvis | high | `jarvis://approvals/unlock` |

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
- A pairing request accepted without a password check (an owner with no
  stored account password) sends no push, so an unauthenticated caller cannot
  trigger one.
- An unlock request needs a device session, so only a signed-in device can
  trigger that push. Each device may create 3 unlock requests per minute. A
  new one supersedes the same device's older pending request: it disappears
  from the approval list and can no longer be approved.
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

## App push to the iOS app (APNs)

Approval events (pairing and unlock requests) can also go straight to the
Jarvis iOS app through Apple's push service, independent of ntfy. Android
keeps using ntfy.

- Every push reads `Jarvis` / `Approval waiting` and carries only the
  approval's deep link. The app fetches the details over the authenticated
  API, and approving still needs the device signature behind Face ID /
  Touch ID; a notification has no actions.
- It goes to the owner's active devices that turned on **Approval
  notifications** in the app, never to the device that asked, at most 8
  devices per event. The same coalescing (one push per kind per 60 s),
  queue, 5 s timeout and retries as ntfy apply. Headers: `apns-push-type:
  alert`, `apns-priority: 10`, `apns-expiration` two minutes ahead (the
  approval expires by then) and an `apns-collapse-id` per kind.
- A device registers its token with `PUT /v1/devices/self/push`
  (`{"apns_token": "<hex>", "apns_env": "sandbox"|"production"}`) and
  removes it with `DELETE`. Both only touch the calling device, at most 6
  writes per minute. Revoking a device deletes its token in the same
  transaction. A token APNs reports as gone (`410`, or `400
  BadDeviceToken`) is deleted.
- Core only pushes to tokens registered for the configured environment.
- Logs name the event kind and APNs status only, never a token or device id.
- What leaves the Home Node: an HTTP/2 request to Apple
  (`api.sandbox.push.apple.com` or `api.push.apple.com`) with the device
  token, app id and the static text. Apple sees that and the timing.
- A missing or invalid setting is logged at startup (`app push disabled`)
  and leaves app push off; Core keeps running.

### Sideloading and the app id

AltStore and SideStore re-sign the app with the owner's own Apple team:

- They register the app as `com.hawkeynl.jarvis.<TEAMID>` (the bundle id
  plus the team id), not `com.hawkeynl.jarvis`. The APNs topic is that
  effective id.
- They sign with a **development** provisioning profile and take the
  entitlements from that profile, not from the app. The profile only carries
  `aps-environment` after the owner enables Push Notifications on that app
  id (step 3), and its value is then `development`: use the APNs
  **sandbox**.
- Neither tool turns the Push Notifications capability on by itself. Not yet
  verified on a device: whether a later refresh by the tool keeps it on. If
  approval pushes stop after a refresh, check the capability (step 3).

This needs a paid Apple Developer account; a free account cannot use push.

### Owner setup, in order

1. **APNs key.** Apple Developer portal: Certificates, Identifiers &
   Profiles → Keys → **+**. Name it (for example `Jarvis APNs`), enable
   **Apple Push Notifications service (APNs)**, register it and download
   `AuthKey_<KEYID>.p8`. Apple offers the download only once: keep the file
   in the password manager. Note the 10-character **Key ID**; the
   10-character **Team ID** is under Membership details. One key works for
   every app id of the team, in both environments.
2. **Install with the paid team.** Sign in to AltStore or SideStore with the
   Apple account of the paid team and install Jarvis (or refresh it once).
   This registers the app id `com.hawkeynl.jarvis.<TEAMID>`.
3. **Push capability.** Portal → Identifiers → that app id → enable **Push
   Notifications** → Save. No push certificate is needed; the key from
   step 1 replaces it.
4. **Re-sign.** In AltStore or SideStore, refresh Jarvis so it fetches a new
   profile that includes `aps-environment`. If turning on Settings →
   **Approval notifications** in Jarvis reports a missing push entitlement,
   check step 3 and refresh again.
5. **Core settings.** Add to `/etc/jarvis/core.env`:

   ```bash
   JARVIS_APNS_TEAM_ID=<TEAMID>
   JARVIS_APNS_KEY_ID=<KEYID>
   JARVIS_APNS_TOPIC=com.hawkeynl.jarvis.<TEAMID>
   JARVIS_APNS_ENVIRONMENT=sandbox
   ```

   `production` is only for a build signed with a distribution (ad hoc or
   App Store) profile, which this project does not use.
6. **Signing key.** The key is a Core secret like the provider credentials:
   one line with the base64 body of the `.p8` file in
   `/etc/jarvis/secrets/apns.env`, `root:jarvis 0640` (checked by
   `verify-home-node.sh` for every file in that directory; loaded by
   `jarvis-core.service` through
   `EnvironmentFile=-/etc/jarvis/secrets/apns.env`). Copy the `.p8` file to
   the Home Node over SSH, then:

   ```bash
   sudo install -o root -g jarvis -m 0640 /dev/null /etc/jarvis/secrets/apns.env
   { printf 'JARVIS_APNS_KEY_P8='; grep -v -- '-----' AuthKey_<KEYID>.p8 | tr -d '\r\n'; echo; } \
     | sudo tee /etc/jarvis/secrets/apns.env >/dev/null
   shred -u AuthKey_<KEYID>.p8
   ```

   Never paste the key into a chat, ticket or the repository.
7. **Restart and check.**

   ```bash
   sudo systemctl restart jarvis-core
   journalctl -u jarvis-core --since -5min | grep 'app push'
   ```

   Expect `app push enabled`.
8. **Phone.** In Jarvis, turn on Settings → **Approval notifications** and
   allow notifications when iOS asks. Test by requesting
   an unlock from another signed-in device: the iPhone shows "Approval
   waiting".

To stop app push, remove the `JARVIS_APNS_*` lines from `core.env`, delete
`/etc/jarvis/secrets/apns.env` and restart Core. Stored tokens stay unused;
turning the toggle off in the app deletes its token. Revoking the key in the
Apple Developer portal stops every push signed with it.

## Rollback

Remove `JARVIS_NOTIFY_NTFY_URL` from `/etc/jarvis/core.env` and
`sudo systemctl restart jarvis-core`. To remove ntfy as well:
`sudo systemctl disable --now ntfy`, comment the ntfy block in the installed
Caddyfile again and `sudo systemctl reload caddy`, then delete
`/etc/jarvis/secrets/ntfy.env` and restart Core again.
