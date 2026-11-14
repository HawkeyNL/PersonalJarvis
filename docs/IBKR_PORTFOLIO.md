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

## Status

`GET /v1/trading/ibkr/status` (owner session) says whether the sync works
and, if not, what to fix. It always answers `200` once authenticated: "off"
and every failure are statuses. Core collects it at most once per 5 seconds
for all callers; collecting reads the broker unit through `systemctl show`,
asks the broker its in-memory status (3 s timeout, never an IBKR call) and
reads the time of the stored portfolio (10 s timeout). Every text in it is a
fixed constant; codes the broker sends are mapped onto a fixed list
(`unknown` otherwise). It never holds a token, credential, account id, path
or log line.

```json
{
  "checked_at": "RFC 3339",
  "overall": "ok | info | warn | error | off",
  "sync": {
    "enabled": true,
    "state": "off | refused | starting | ok | failing | waiting_for_broker",
    "last_success_at": "RFC 3339 | null",
    "last_attempt_at": "RFC 3339 | null",
    "next_attempt_at": "RFC 3339 | null",
    "consecutive_failures": 0,
    "last_error": { "code": "...", "at": "RFC 3339" }
  },
  "broker": {
    "service": "active | activating | inactive | failed | deactivating | not_found | unknown",
    "enabled_at_boot": "true | false | null",
    "credentials": "present | missing | unknown",
    "reachable": "true | false | null (not asked: sync off or configuration refused)",
    "started_at": "RFC 3339 | null"
  },
  "token": {
    "state": "none | valid | refresh_due | expired | rejected",
    "usable": true,
    "refreshed_at": "RFC 3339 | null",
    "expires_at": "RFC 3339 | null",
    "last_refresh_attempt_at": "RFC 3339 | null",
    "last_refresh_error": "code | null"
  },
  "ibkr": {
    "last_ok_at": "RFC 3339 | null",
    "last_error": { "code": "...", "at": "RFC 3339" },
    "rate_limited_until": "RFC 3339 | null",
    "brokerage_session": "open | closed | competing"
  },
  "problems": [
    { "code": "...", "severity": "info | warn | error", "summary": "...", "commands": ["..."] }
  ]
}
```

Every field is always present; `last_error` is `null` when there was none,
and `token` and `ibkr` are `null` when the broker did not answer the status
request. `sync.last_success_at` also counts the stored portfolio, so it
survives a Core restart. `credentials` is the result of the broker unit's
credential condition at its last start.

Sync error codes: `broker_not_running`, `broker_unavailable`, `broker_busy`,
`broker_timeout`, `ibkr_auth`, `ibkr_rate_limited`, `ibkr_failed`,
`broker_protocol`, `account_unsupported`, `data_refused`, `store_failed`; a
refused configuration reports `socket_path_invalid`,
`broker_account_missing` or `broker_account_unsafe`. Broker codes:
`auth_rejected`, `not_authenticated`, `rate_limited`, `upstream`, `busy`,
`invalid_input`.

`problems` holds at most 8, most severe first. `overall` is `off` while the
sync is off, otherwise the most severe problem, or `ok`. The commands are
for the owner to copy; Core and the apps never run them.

| Code | Severity | When |
|---|---|---|
| `sync_disabled` | info | The sync is off (the only problem then). |
| `config_refused` | error | Turned on, but Core refused the configuration; the commands depend on the refusal code. |
| `broker_not_installed` | error | The broker unit does not exist. |
| `credentials_missing` | error | The unit's credential condition failed. |
| `broker_not_enabled` | error | The unit does not start at boot. |
| `broker_failed` | error | The unit failed. |
| `broker_unreachable` | error | No usable answer from the broker, unless a unit problem explains it while the unit is not running. A reply this Core cannot read, also from a newer broker, counts as no answer. |
| `broker_restart_needed` | warn | The running broker predates the status request. |
| `auth_rejected` | error | IBKR rejected the current token or the last token request. |
| `token_expired` | info | The token expired; the next request renews it. |
| `session_competing` | warn | Another IBKR session competes. |
| `rate_limited` | warn | IBKR rate limiting is active. |
| `account_unsupported` | error | The last sync failed on the account (not one EUR cash account). |
| `sync_failing` | warn, error from 3 failures in a row | The last sync failed for another reason. Waiting for a missing broker socket is not counted and resets the count, like the backoff. |
| `sync_stale` | warn | The last good sync is more than 15 minutes old. |
| `first_sync_pending` | info | No sync finished yet and nothing is stored. |
| `core_store_unavailable` | error | Core could not read the stored portfolio. |
