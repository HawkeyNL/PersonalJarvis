# Caddy ingress for Jarvis Core

`Caddyfile` is the public HTTPS ingress template. TLS ends at Caddy, which
forwards API traffic to Core on the same host.

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

The Core unit creates `/run/jarvis-core` as a systemd `RuntimeDirectory` and
sets it to `jarvis:caddy`, mode `2750`. The setgid bit gives the socket that
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

Requires a Core release that contains this feature. The TCP listener on
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

A Core release from before this feature has no socket. If Caddy points at
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
