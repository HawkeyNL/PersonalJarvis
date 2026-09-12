# Public installation page

`/downloads` and `/downloads/` serve one static page through the existing Caddy
HTTPS origin on TCP 443. No JavaScript, browser authentication, telemetry, embedded
credentials, directory browsing, or public update API is added.

This is an **installation landing page**, not an anonymous alias for the Home
Node update mirror. Its download links intentionally lead to the canonical public
`HawkeyNL/PersonalJarvisApp` GitHub Releases. The files are not hosted by the Home
Node in this implementation. Until a public client release exists, the page
provides no downloadable build. iOS remains local Xcode installation only.

Authenticated native app updates continue through `/v1/app-updates/**`. Never
point a Caddy file-server root at `/var/lib/jarvis/app-updates`, `/etc/jarvis`, a
checkout, or a home directory. Caddy cannot sandbox symlink targets: keep the
static directory root-owned and install a regular `index.html`, not a symlink.

## Owner deployment after review

This source change does not install files, reload Caddy, modify firewall rules,
or deploy a Core release. Apply through the Home Node's existing trusted owner
administration process and required approval. Do not replace an existing
Caddyfile blindly: preserve reviewed local hostname/TLS settings and unrelated
sites. Add only the exact `@downloads` / `handle @downloads` block from the
reviewed template, and put the existing Core proxy in its fallback `handle`.

From the reviewed checkout, install only the static page:

```sh
sudo install -d -o root -g root -m 0755 /usr/share/jarvis/downloads
sudo install -o root -g root -m 0644 deploy/caddy/downloads/index.html /usr/share/jarvis/downloads/index.html
```

Validate the complete proposed Caddyfile using the existing trusted hostname
environment before activating it. Keep a recoverable copy of the previous
configuration. After approval, reload through the existing Caddy service and
verify `/downloads` in a browser. Do not install Nginx or open additional ports.
Verify an unauthenticated `/v1/app-updates/capability` request remains rejected;
use an enrolled native client to check authenticated updates. A rootless routing
test with a fake protected upstream does not replace those real-host checks.

## Tests

```sh
bash deploy/caddy/tests/test-caddy-template.sh
python3 deploy/caddy/tests/test-public-downloads.py
```

The second test requires Caddy. It starts a separate loopback-only process with
its admin API and HTTPS automation disabled, uses disposable state, and never
contacts or reloads production Caddy. It proves the exact public paths serve the
page while update, websocket and unexpected file paths still reach the protected
upstream. Existing Core tests remain authoritative for actual bearer validation.
