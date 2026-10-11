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
`JARVIS_TRUSTED_PROXY_HOPS` / `JARVIS_TRUSTED_PROXY_IPS` behaviour.

### How access is granted

The Core unit creates `/run/jarvis-core` as a systemd `RuntimeDirectory` and
sets it to `jarvis:caddy`, mode `2750`. The setgid bit gives the socket that
Core creates (mode `0660`) the `caddy` group, so Caddy can connect without Core
joining the `caddy` group or Caddy joining the `jarvis` group. Core refuses to
start the socket if the directory is not owned by Core or is accessible to
other users. Trust is decided by the peer uid, not by the file mode.

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
   lines as they are; they are ignored while the socket is set and keep
   rollback a one-line change.
4. Restart Core, then reload Caddy straight away:
   `sudo systemctl restart jarvis-core.service && sudo systemctl reload caddy`.
   Between the two commands Caddy still uses TCP, which Core no longer trusts,
   so remote authenticated requests are refused for those few seconds.
5. Verify: `stat -c '%U:%G %a' /run/jarvis-core/api.sock` prints
   `jarvis:caddy 660`, and a client can sign in and use the API through the
   public hostname.

### Roll back

1. In `/etc/caddy/Caddyfile`, make `reverse_proxy 127.0.0.1:8080` the active
   upstream again and comment out the Unix line.
2. Remove `JARVIS_API_UNIX_SOCKET` from `/etc/jarvis/core.env` (or leave it
   empty). This step is required: while it is set, Core does not trust
   forwarding headers on TCP.
3. `sudo systemctl restart jarvis-core.service && sudo systemctl reload caddy`.

### Next step

A later release makes the Unix socket the only API path for Caddy: it removes
the TCP API listener and with it all TCP-based forwarding trust
(`JARVIS_TRUSTED_PROXY_*`). Enabling the socket now is the migration path to
that release.
