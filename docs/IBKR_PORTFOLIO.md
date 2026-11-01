# IBKR portfolio sync

Core can show the owner's IBKR account in the app: positions, cash, net
liquidation, a daily value chart and price bars of held instruments. It is
**read-only**: there is no order, mandate or signer path, and Core never
holds IBKR credentials. Only the separate `jarvis-ibkr-broker` holds them;
Core asks it a fixed set of read requests over its local Unix socket and
refuses a socket that is not served by the broker's own account
(`jarvis-ibkr`).

It is **off by default** (`JARVIS_IBKR_PORTFOLIO_SYNC_ENABLED=false`). While
it is off, or the broker account does not exist, the trading routes answer
`503` ("not configured") and Core makes no broker call.

Scope: one IBKR **cash account** with base currency **EUR**, and positions
in EUR. A position in another currency (or without one) is not shown: the
sync leaves it out, the API reports it in `skipped_positions` and the Core log
warns once whenever that set changes. Anything else (a second account,
another base currency, a value Core cannot parse, also in a skipped position)
refuses the sync: the last good data stays and shows as stale in the app, and
the Core log says why.

## Enabling it (owner)

1. Set up and start the broker first: `sudo jarvis-credentials set
   ibkr-oauth`, then `sudo systemctl enable --now jarvis-ibkr-broker.service`.
2. Set `JARVIS_IBKR_PORTFOLIO_SYNC_ENABLED=true` in `/etc/jarvis/core.env`
   and restart Core. `JARVIS_IBKR_BROKER_SOCKET` only needs setting if the
   broker uses another socket than `/run/jarvis-ibkr-broker/broker.sock`.
3. The Core log shows `IBKR portfolio synced` after the first good sync, or
   the reason it was refused. `IBKR portfolio sync disabled` at startup means
   the configuration was refused (for example, the `jarvis-ibkr` account is
   missing); Core keeps running either way.

To turn it off, set the flag back to `false` and restart Core. The stored
portfolio stays in the database but is no longer served.

## What the sync does

- Right after start and then every 5 minutes: read the account, its positions
  and its ledger through the broker, validate every value and replace the
  stored portfolio in one transaction (schema 0015). Each broker call is
  bounded by a timeout of 65 s.
- The net liquidation of the last good sync of each UTC day is kept as one
  history point; points older than 3650 days are pruned.
- A failed sync is retried after 10, 20 and then every 30 minutes. While the
  broker socket does not exist, the sync waits at the normal interval and
  logs that once (a socket left behind by a crashed broker counts as a
  failure and backs off). Every outcome is logged only when it changes.

## API (owner session, like every `/v1` route)

The shapes follow the app's trading contract. Decimal values are strings;
every response carries `source` (`IBKR`) and `as_of` (the last good sync,
or the bars read). `delay_minutes` is left out because the broker does not
report the price delay; the app shows it as "delay unknown".

- `GET /v1/trading/portfolio`: `base_currency`, `net_liquidation`, `cash`
  and `positions` (largest market value first, with `weight_pct` as a share
  of net liquidation). When positions were left out for their currency, also
  `skipped_count` and `skipped_positions` (at most 50, sorted by symbol:
  `symbol` and, if IBKR reported a three-letter code, `currency`); both are
  absent otherwise. `net_liquidation` stays as IBKR reports it and includes
  the skipped positions, so the shown weights then add up to less than the
  non-cash share of the account.
- `GET /v1/trading/portfolio/history?range=1M|3M|1Y|ALL`: daily points,
  oldest first.
- `GET /v1/trading/instruments/{conid}/bars?range=1D|1W|1M|3M|1Y|5Y`: bars
  read live through the broker (5-minute bars for `1D`, hourly for `1W`,
  daily up to `1Y`, weekly for `5Y`). Only instruments the account holds;
  other contract ids answer `404`.

Errors: `503` when the sync is off, has no data yet or the broker cannot be
reached; `400` for an invalid range or contract id; `500` for a Core
failure.
