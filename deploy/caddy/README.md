# Caddy ingress for Jarvis Core

`Caddyfile` is the public HTTPS ingress template. TLS ends at Caddy, which
forwards API traffic to Core on the same host.

## Admin API on a Unix socket

The template's global options set `admin unix//run/caddy/admin.sock`, so
Caddy's admin API is not on TCP `127.0.0.1:2019`, where every local user and
service could replace the running configuration. The drop-in
`caddy.service.d-jarvis-admin.conf` makes systemd create `/run/caddy` as
`caddy:caddy` with mode `0700` at each start. Caddy creates the socket with the
service umask (`0755` with the default `0022`); connecting needs write access,
so only `caddy` and root can use it. The Ubuntu package (Caddy 2.6) has no
socket file mode option, so the directory is what restricts access.

`systemctl reload caddy` keeps working: the packaged unit runs
`caddy reload --config /etc/caddy/Caddyfile --force`, which reads the admin
address from the file it loads. Other `caddy` commands that talk to a running
instance need root and `--config /etc/caddy/Caddyfile` or
`--address unix//run/caddy/admin.sock`.

### Shared Caddy instance

The admin endpoint belongs to the whole Caddy process, not to one site. A
Caddyfile has a single global options block, at the top. If the installed
`/etc/caddy/Caddyfile` also serves other sites or products, this change applies
to them too: anything that uses `http://localhost:2019` stops working. That
includes scripts or services that post configuration, `caddy-api.service` or
`caddy run --resume` setups, and Prometheus scraping of the admin `/metrics`
endpoint. Check this before the rollout. Add the `admin` line to the existing
global options block; never add a second block.

### Rollout

Run these as the owner; they restart Caddy, which drops connections to every
site it serves for about a second. Certificates are reused from storage.

1. Check what uses the admin API and whether other sites share the instance:
   - `sudo ss -ltnp 'sport = :2019'` should list only `caddy`;
   - `systemctl cat caddy` shows the unit and any existing drop-ins (look for
     `--resume`, another `RuntimeDirectory=` or `caddy-api.service`);
   - `sudo grep -n -E '2019|admin|^[^#[:space:]].*\{' /etc/caddy/Caddyfile`
     lists global options and all site blocks;
   - search the other products' configs, scripts and monitoring for `2019`;
   - `ls -ld /run/caddy` should fail with "No such file or directory". If it
     exists, find out who uses it before continuing: systemd takes it over and
     deletes it whenever Caddy stops.
2. Back up: `sudo cp -a /etc/caddy/Caddyfile /etc/caddy/Caddyfile.bak-admin-tcp`.
3. Install the drop-in and load it (this alone changes nothing at runtime):

   ```sh
   sudo install -d -o root -g root -m 0755 /etc/systemd/system/caddy.service.d
   sudo install -o root -g root -m 0644 deploy/caddy/caddy.service.d-jarvis-admin.conf \
     /etc/systemd/system/caddy.service.d/jarvis-admin.conf
   sudo systemctl daemon-reload
   ```

4. With `sudoedit /etc/caddy/Caddyfile`, add `admin unix//run/caddy/admin.sock`
   to the global options block.
5. Validate with the hostname environment that Caddy's service loads:
   `sudo sh -c 'set -a; . /etc/jarvis/public.env; caddy validate --config /etc/caddy/Caddyfile'`.
6. Restart, do not reload: `sudo systemctl restart caddy`. The running instance
   still listens on TCP 2019, while `systemctl reload` would send the new
   configuration to the socket address in the new file, which does not exist
   yet. That reload fails and leaves the old configuration running. A restart
   also lets systemd create `/run/caddy`.
7. Verify:
   - `ss -ltn | grep ':2019'` prints nothing;
   - `sudo stat -c '%U:%G %a' /run/caddy` prints `caddy:caddy 700` and
     `sudo ls -l /run/caddy/admin.sock` shows a socket owned by `caddy`;
   - `sudo journalctl -u caddy -n 50` shows `admin endpoint started` with
     `unix//run/caddy/admin.sock`;
   - `sudo systemctl reload caddy` succeeds (this uses the socket);
   - the Jarvis hostname and every other site on the instance still work.

### Roll back

Restore the backup and restart (a reload cannot work here: it would look for
the admin API on TCP 2019, which is no longer listening):

```sh
sudo cp -a /etc/caddy/Caddyfile.bak-admin-tcp /etc/caddy/Caddyfile
sudo systemctl restart caddy
```

The drop-in is harmless without the `admin` line. To remove it as well:
`sudo rm /etc/systemd/system/caddy.service.d/jarvis-admin.conf`, then
`sudo systemctl daemon-reload` and `sudo systemctl restart caddy`.

## Core API over a Unix socket

> **Before this release is published:** switch the host to the socket
> ([below](#switch-an-existing-host-before-updating)) or pause automatic
> updates with `sudo systemctl stop jarvis-updater.timer`. On a host that has
> not switched, the installed updater tries each such release, which cannot
> start: Core is down for a minute or two every time until the previous
> release is restored. That release then stays staged and later runs stop
> with "release directory already exists"; every newer release repeats this.

Core serves its API to Caddy only on the Unix socket
`/run/jarvis-core/api.sock`. Core honours `X-Forwarded-For` and
`X-Forwarded-Proto` only for connections on that socket whose kernel-reported
peer uid is the `caddy` user, so client attribution (rate limits, login
lockout, bootstrap network policy) and the HTTPS check depend on the kernel
identity of the proxy instead of a loopback address. Forwarding headers on TCP
are never trusted.

In production:

- Core refuses to start unless `JARVIS_API_UNIX_SOCKET=/run/jarvis-core/api.sock`
  is set in `/etc/jarvis/core.env`, and the installer refuses to activate a
  release without that line or without a `caddy` account;
- the loopback TCP listener on `127.0.0.1:8080` answers only `/livez` and
  `/readyz` (for systemd, the updater and the admin tools); every other path
  returns 404;
- the former `JARVIS_TRUSTED_PROXY_HOPS` / `JARVIS_TRUSTED_PROXY_IPS` settings
  no longer exist. Leftover lines are ignored; keep them while a rollback to a
  release from before the optional socket is still possible (see below).

Development and test runs may leave the socket unset; their TCP listener then
serves the full API and trusts no forwarding headers.

Keep the upstream on HTTP/1.1, which is Caddy's default for a plain-HTTP or
Unix-socket upstream: do not add `transport http { versions h2c }` to the
`reverse_proxy`. Core's listeners also accept HTTP/2 cleartext with prior
knowledge (a side effect of the HTTP/2 client Core uses for APNs), but the
API, including its WebSocket upgrade, is served and tested over HTTP/1.1
only.

### How access is granted

Before every start the Core unit creates `/run/jarvis-core` with `install -d`
as `jarvis:caddy`, mode `2750`. It is not a systemd `RuntimeDirectory`: systemd
would reset that to `jarvis:jarvis 0700` before each command. The setgid bit gives the socket that
Core creates (mode `0660`) the `caddy` group, so Caddy can connect without Core
joining the `caddy` group or Caddy joining the `jarvis` group. Production
requires exactly `/run/jarvis-core/api.sock`, the path the Caddyfile uses.
Core opens it only if that
directory is owned by Core, has the `caddy` user's group and the setgid bit,
and grants nothing to other users, only if the new socket ends up in the
`caddy` group, and only if `caddy` is a different account from Core. Trust is
decided by the peer uid, not by the file mode.

Core owns the directory, so a compromised Core process could replace the
socket. That is accepted: such a process already controls every response.

If any of these checks fails, Core does not start at all, including its TCP
listener. Check `sudo journalctl -u jarvis-core.service -n 50` and use the
rollback below.

### Switch an existing host before updating

A host that still proxies to `127.0.0.1:8080` must switch to the socket while
it runs a Core release that already supports the optional socket, before the
updater installs a release that requires it. v0.0.55 cannot be used for this:
its unit leaves `/run/jarvis-core` at `jarvis:jarvis 700`. Switch on v0.0.61,
and switch back to TCP before rolling back to v0.0.55.

1. Pause automatic updates for the switch:
   `sudo systemctl stop jarvis-updater.timer`. Check the runtime directory:
   `stat -c '%U:%G %a' /run/jarvis-core` must print `jarvis:caddy 2750`.
2. Back up: `sudo cp -a /etc/caddy/Caddyfile /etc/caddy/Caddyfile.bak-api-tcp`.
3. In `/etc/caddy/Caddyfile`, inside the final `handle` block, replace
   `reverse_proxy 127.0.0.1:8080` with
   `reverse_proxy unix//run/jarvis-core/api.sock`. Validate with
   `sudo sh -c 'set -a; . /etc/jarvis/public.env; caddy validate --config /etc/caddy/Caddyfile'`,
   but do not reload yet.
4. Add `JARVIS_API_UNIX_SOCKET=/run/jarvis-core/api.sock` to
   `/etc/jarvis/core.env` with `sudoedit`. Leave the `JARVIS_TRUSTED_PROXY_*`
   lines for now; they keep the rollback below a small change.
5. Reload Caddy first, then restart Core:
   `sudo systemctl reload caddy && sudo systemctl restart jarvis-core.service`.
   Until Core has created the socket, Caddy answers API requests with a short
   502. The unit is `Type=exec`, so `systemctl restart` returns before Core has
   finished starting.
6. Verify:
   - `sudo journalctl -u jarvis-core.service -n 50` shows
     `jarvis-api listening on Unix socket` and no startup error;
   - `curl --fail http://127.0.0.1:8080/readyz` succeeds;
   - `stat -c '%U:%G %a' /run/jarvis-core/api.sock` prints `jarvis:caddy 660`;
   - `sudo jarvis health` passes "Caddy does not proxy to Core over TCP" and
     "Caddy reaches Core through its API socket";
   - a client can sign in and use the API through the public hostname.
7. Resume automatic updates: `sudo systemctl start jarvis-updater.timer`.

What happens when this is skipped:

- Without the socket line, the new Core exits at startup with a configuration
  error. The installer refuses before activating; the updater's `/readyz`
  check fails and it restores the previous release automatically, after a
  minute or two without Core. The verified release stays staged in
  `/opt/jarvis/releases`, so later automatic runs stop with "release directory
  already exists". After the switch above, activate it explicitly:
  `sudo /usr/local/libexec/jarvis/update-core-release --rollback-version vX.Y.Z`
  (this selects any verified, staged release with the active schema).
- With the socket line but Caddy still on `127.0.0.1:8080`, Core starts and
  `/readyz` passes, but every API request through Caddy gets a 404. Releases
  that support the optional socket already rejected that combination (no
  trusted HTTPS proxy on TCP), so it is not new. Finish steps 3 and 5;
  `verify-home-node.sh` reports it.

### Roll back

- To a Core release that supports the optional socket with a working socket
  directory (v0.0.61 or later): roll back Core as usual. Caddy can stay on the
  socket; that release still serves the full API there.
- To the TCP upstream (only with such a release active): restore
  `/etc/caddy/Caddyfile.bak-api-tcp` (or make `reverse_proxy 127.0.0.1:8080`
  the active upstream again), remove `JARVIS_API_UNIX_SOCKET` from
  `/etc/jarvis/core.env` and make sure the previous `JARVIS_TRUSTED_PROXY_*`
  lines are present. Then restart Core first and reload Caddy:
  `sudo systemctl restart jarvis-core.service && sudo systemctl reload caddy`.
  A release that requires the socket must not be active in this state; it
  would not start.

### Core release rollbacks

A Core release from before the optional socket has no socket. If Caddy points
at `unix//run/jarvis-core/api.sock` when such a release is activated, every API
request gets a 502 until the Caddyfile is switched back. Switch Caddy back to
the TCP upstream (and reload it) before rolling Core back to such a release.
That release also needs `JARVIS_TRUSTED_PROXY_HOPS=1` and
`JARVIS_TRUSTED_PROXY_IPS=127.0.0.1` in `/etc/jarvis/core.env`; without them
every authenticated request through Caddy fails with 401.
