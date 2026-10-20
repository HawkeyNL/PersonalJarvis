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

## Optional: Core API over a Unix socket

As defense in depth, Core can additionally serve its API on a Unix socket that
only Caddy can reach. With the socket enabled, Core honours `X-Forwarded-For`
and `X-Forwarded-Proto` only for connections on that socket whose
kernel-reported peer uid is the `caddy` user. Requests on the TCP listener are
then always treated as direct local requests, so client attribution (rate
limits, login lockout, bootstrap network policy) and the HTTPS check depend on
the kernel identity of the proxy instead of a loopback address.

The socket is off by default. Without `JARVIS_API_UNIX_SOCKET`, Core keeps the
`JARVIS_TRUSTED_PROXY_HOPS` / `JARVIS_TRUSTED_PROXY_IPS` behaviour. With it,
those two settings are ignored for trust but still validated at startup.

### How access is granted

Before every start the Core unit creates `/run/jarvis-core` with `install -d`
as `jarvis:caddy`, mode `2750`. It is not a systemd `RuntimeDirectory`: systemd
would reset that to `jarvis:jarvis 0700` before each command. The setgid bit gives the socket that
Core creates (mode `0660`) the `caddy` group, so Caddy can connect without Core
joining the `caddy` group or Caddy joining the `jarvis` group. The socket path
must be directly inside `/run/jarvis-core`. Core opens it only if that
directory is owned by Core, has the `caddy` user's group and grants nothing to
other users, and only if `caddy` is a different account from Core. Trust is
decided by the peer uid, not by the file mode.

Core owns the directory, so a compromised Core process could replace the
socket. That is accepted: such a process already controls every response.

If any of these checks fails, Core does not start at all, including its TCP
listener. Check `sudo journalctl -u jarvis-core.service -n 50` and use the
rollback below.

### Enable

Requires v0.0.61 or later: v0.0.55 leaves `/run/jarvis-core` at
`jarvis:jarvis 700`, so Core refuses the socket and does not start. The TCP listener on
`127.0.0.1:8080` keeps serving everything, so local health checks are
unchanged.

1. Check the runtime directory after the release is active:
   `stat -c '%U:%G %a' /run/jarvis-core` must print `jarvis:caddy 2750`.
2. In `/etc/caddy/Caddyfile`, inside the final `handle` block, comment out
   `reverse_proxy 127.0.0.1:8080` and uncomment
   `reverse_proxy unix//run/jarvis-core/api.sock`. Validate with
   `sudo caddy validate --config /etc/caddy/Caddyfile`, but do not reload yet.
3. Add `JARVIS_API_UNIX_SOCKET=/run/jarvis-core/api.sock` to
   `/etc/jarvis/core.env` with `sudoedit`. Leave the `JARVIS_TRUSTED_PROXY_*`
   lines as they are; they keep rollback a one-line change.
4. Reload Caddy first, then restart Core:
   `sudo systemctl reload caddy && sudo systemctl restart jarvis-core.service`.
   Until Core has created the socket, Caddy answers API requests with a short
   502. The reverse order would instead let Core reject Caddy's TCP requests
   as untrusted (401s and one shared rate-limit bucket). The unit is
   `Type=exec`, so `systemctl restart` returns before Core has finished
   starting.
5. Verify:
   - `sudo journalctl -u jarvis-core.service -n 50` shows
     `jarvis-api listening on Unix socket` and no startup error;
   - `curl --fail http://127.0.0.1:8080/readyz` succeeds;
   - `stat -c '%U:%G %a' /run/jarvis-core/api.sock` prints `jarvis:caddy 660`;
   - a client can sign in and use the API through the public hostname.

### Roll back

1. In `/etc/caddy/Caddyfile`, make `reverse_proxy 127.0.0.1:8080` the active
   upstream again and comment out the Unix line.
2. Remove `JARVIS_API_UNIX_SOCKET` from `/etc/jarvis/core.env` (or leave it
   empty). This step is required: while it is set, Core does not trust
   forwarding headers on TCP.
3. Restart Core first, then reload Caddy:
   `sudo systemctl restart jarvis-core.service && sudo systemctl reload caddy`.

### Core release rollbacks

A Core release from before v0.0.61 has no usable socket. If Caddy points at
`unix//run/jarvis-core/api.sock` when such a release is activated, every API
request gets a 502 until the Caddyfile is switched back. Switch Caddy back to
the TCP upstream (and reload it) before rolling Core back to such a release.

### Next step

A later release makes the Unix socket the only API path for Caddy: it removes
the TCP API listener and with it all TCP-based forwarding trust
(`JARVIS_TRUSTED_PROXY_*`). Enabling the socket now is the migration path to
that release. The same caution applies there in reverse: Caddy must already
use the Unix upstream before that release is activated, and rolling back from
it to a release before this feature needs the TCP upstream restored first.
