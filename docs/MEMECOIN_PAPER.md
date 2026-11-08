# Memecoin paper trader

Core can run the on-chain memecoin strategies as a **paper** simulation: it
reads public market data, applies the hard filters and several typed
strategies from `jarvis-onchain` side by side, and simulates fills, fees and
failed attempts. It has no
wallet, no signer and no execution path, and no LLM output reaches it. Results
are stored in schemas 0014 and 0017 and shown through `/v1/trading/meme/*`.

It is **off by default** (`JARVIS_MEMECOIN_PAPER_ENABLED=false`). Any
configuration problem at startup leaves it off and logs why; Core keeps
running.

## Enabling it (owner)

1. Write the paper profile (below) to `/etc/jarvis/memecoin-paper.json`,
   owned by root, with the group of `/etc/jarvis` and mode `0640`. Production
   refuses any other owner, mode or a symlink. No API writes this file.
2. Put the market-data secrets in `/etc/jarvis/secrets/market-data.env`, with
   the same owner and mode as the other provider files there. The Core unit
   loads it as an optional `EnvironmentFile`, like the LLM provider keys:
   - `JARVIS_MARKET_SOLANA_RPC_A_URL` and `JARVIS_MARKET_SOLANA_RPC_B_URL`:
     two HTTPS Solana RPC endpoints on **different** hosts from the
     market-data allowlist (`api.mainnet-beta.solana.com`,
     `mainnet.helius-rpc.com`, `solana-mainnet.g.alchemy.com`). Every read is
     made on both and must agree. The URLs usually contain a provider key;
     Core redacts them in its logs.
   - `JARVIS_MARKET_JUPITER_API_KEY` (optional): without it, quotes are
     paced to one per 2.1 s.
3. Set `JARVIS_MEMECOIN_PAPER_ENABLED=true` in `/etc/jarvis/core.env` and
   restart Core. The log shows `memecoin PAPER trader running` or the reason
   it stayed off; the overview then reports `running` or a `last_error`.

`jarvis-credentials` does not manage `market-data.env` yet; a provider that
reads these values from the TTY and probes them read-only is a follow-up.
Until then the owner writes the file by hand.

## Paper profile

A JSON object; unknown fields are refused. Amounts are decimal strings, ratios
in basis points, durations in seconds (at most 30 days). The repository ships
no values on purpose.

- `watchlist`: 1–20 distinct base58 mints to observe.
- `discovery` (optional, default `true`): also observe mints from pump.fun
  migrations (below). `false` limits the trader to the watchlist.
- `goal_multiple`: target multiple of the stake (above 1), for progress only.
- `stake_usd`, `max_position_usd`, `max_open_positions` (1–10): simulation
  inputs only, never limits for live trading. The stake must cover
  `max_position_usd × max_open_positions`.
- `filter`: `min_liquidity_usd`, `min_pool_age_secs`, `max_top_holders_share`
  (fraction), `max_sell_impact_bps`, `allowed_extensions` (snake_case
  Token-2022 extension names), `max_snapshot_age_secs`,
  `min_lp_lock_remaining_secs`.
- `strategy` (optional, **deprecated**): `version` and the `StrategyParams`
  fields of `jarvis-onchain` (`*_secs` for durations); validated against the
  code-fixed bounds, with `version` below 1 000 000. This single strategy of
  older profiles no longer enters. It is still read so the positions it
  opened keep exiting by its rules; drop it once none are held.
- `costs`: `dex_fee_bps`, `network_fee_usd`, `priority_fee_usd`,
  `failure_rate` (fraction, below 1).

While the profile has a `strategy`, everything except `watchlist`,
`discovery` and `goal_multiple` is recorded with its version. Changing any of
it without raising `strategy.version` keeps the trader off; that includes
rewriting `"25"` as `"25.0"`, since decimals keep their scale. The filter
rules, costs and sizes apply to every strategy and are not versioned with the
typed strategies.

## Strategies

At startup Core records the seed strategies `a-survivor-momentum`,
`b-base-reclaim` and `c-established-trend` (families A, B and C of
`jarvis-onchain::config`) as versions 1 000 001 to 1 000 003, author `seed`,
and enables each one it creates. Versions are unique across strategies;
typed versions start above 1 000 000 and profile versions stay below, so
they never collide. Seeding is idempotent: an existing strategy keeps its
active version and its enabled or owner-disabled state. A seed whose filter
overrides would loosen the profile's filter rules, or whose version is
already recorded with other content, is left out and logged; the others
still run.

Each tick, every **enabled** strategy's active version is evaluated, in id
order:

- The hard filter runs with the profile's rules tightened by the version's
  overrides. A candidate row is written per mint and strategy version, when
  that verdict changes.
- The entry conditions read signals computed from our own recorded
  observations: every price and liquidity seen in the last 25 hours plus the
  daily ones before, at most 2000 points, ending with the snapshot an entry
  would fill at.
- The entry size is the version's share of `max_position_usd`, capped by its
  share of pool liquidity.
- Limits: the profile's `max_open_positions` and one position per mint
  across all strategies; per strategy an equal share of `max_open_positions`
  among the enabled strategies (at least one), its `max_entries_per_day` per
  UTC day, and its `token_cooldown_secs` since it last left that mint.
- The first strategy whose limits and signal allow it makes the one entry
  attempt for that mint this tick; its trip and any failed-attempt cost carry
  that strategy's version.

A **disabled** strategy makes no new entries; its open positions still exit.
Every position exits by the rules of the version that opened it: liquidity
drop, stop loss, trailing stop (the version's `trailing_stop_bps` below the peak executable value, once
that peak was above cost; the peak is stored with the position, so a restart
keeps it), take profit and time stop. Positions of a legacy profile version
exit by the parameters recorded for that version. A version that no longer
fits the profile's filter rules only exits. A position whose version has no
usable rules recorded gets only the time stop, after 30 days, and is logged
every tick until then.

The overview reports the whole paper book (every strategy). The hourly gate
snapshot is written per strategy, for its active version; one that fails is
logged and skipped until the next hour. Typed versions are only recorded
above 1 000 000, with parameters that name the same strategy, version,
parent and family as their row. Trades, net result
(after failed-attempt fees and AI cost), expectancy, win rate and maximum
drawdown per strategy, over all its versions, are available to Core as
`store::strategy_performance`; there is no route for them yet.

## What a tick does

Every 60 s: value held positions and simulate exits (also while paused);
then, if the stored ledger replays exactly, a strategy is enabled and the
trader is not paused, poll discovery (at most every 2 minutes), observe up to
5 mints (watchlist first, then discovered mints; each at most every 15
minutes, through the per-provider pacing of `jarvis-market-data`), run the
filter and the strategies, and simulate entries; finally report from the
stored ledger. The price and liquidity of every held, watchlist and observed
mint are recorded each tick for the signals. Discovered mints are only seen
when they are observed as candidates, so their series are sparser than the
watchlist's. A
write failure stops the tick before anything is simulated on it.

- An exit that fails or cannot be priced is retried on later ticks; after 10
  such attempts the position is marked `stuck` (shown in the overview), is
  no longer retried and keeps its slot. Failed-attempt fees are summed per
  day, so they cannot grow the ledger without bound.
- If the stored ledger no longer replays exactly or exceeds 100 000 rows,
  entries and the report stop until the owner investigates; exits continue.
  The overview shows a fixed reason as `last_error` (details are in the
  log, reported once).
- A ledger dated more than five minutes ahead of the clock stops the whole
  tick, so events are never stamped into the future.
- If the pause state cannot be read, that tick makes no entries.

A candidate row is written only when a mint's verdict changes. Daily
housekeeping prunes, in bounded batches:

- candidate rows and the hourly gate snapshots after 30 days;
- strategy-agent decisions after 90 days;
- price and liquidity observations after 72 hours, except the first one of
  each mint and UTC day, which is kept for 60 days;
- universe entries (recently migrated tokens) 7 days after the migration.

Trips, costs, strategies and strategy versions are kept for the paper gate.

**LP status:** the LP of the main pool (the deepest DexScreener pair; other
pools of the token are not checked) is read on chain, only for PumpSwap,
Raydium AMM v4 and Raydium CPMM pools. The pool account must be owned by one
of those programs and hold the pair's two mints. LP counts as burned when the
LP mint supply is zero, or when at most 1% of it sits outside accounts owned
by the incinerator or, for PumpSwap, the pool itself. Everything else (other
venues such as CLMM or DLMM, LP in a wallet, a Raydium pool authority or a
third-party locker, unreadable data, the two RPC providers disagreeing) is
rejected with `lp_not_secured`; time locks are never recognised. The verified
pool vaults are left out of the top-holder share.

## Discovery

When a pump.fun bonding curve completes, pump.fun migrates it to a PumpSwap
pool. Every migration writes to the pump.fun migration account
(`39azUYFWPz3VHgKCf3VChUwbpURdCHRxjWVowf5jUJjg`, the `withdraw_authority` of
pump.fun's Global account), so discovery polls `getSignaturesForAddress` on
that account and reads each new transaction with `getTransaction`. A
transaction counts only if a top-level pump.fun `migrate` or `migrate_v2`
instruction succeeded and called PumpSwap; a repeated migrate on an already
migrated curve is ignored. Its mint and pool are kept.

- At most 200 signatures per poll and 25 transactions read per poll; the
  rest is read on the next poll. A poll is cut off after 15 s.
- Failed transactions and signatures older than 72 hours are skipped
  unread, and a transaction that does not parse is skipped. A malformed
  signature list, a rate limit or a transport error ends the poll, which
  resumes at the same signature later.
- At most 2000 mints are kept, each once, for 72 hours after its migration
  (the oldest is dropped first). A mint is offered as a candidate from
  30 minutes after its migration on. With 5 observations per tick, shared
  with the watchlist, a busy day can leave some unobserved.
- Discovered mints are stored in the universe table after each poll, and
  those within the 72-hour window are loaded again at startup, so a restart
  keeps them. The poll position is not stored: after a restart discovery
  starts from the newest 200 signatures.

Discovery reads only the primary RPC provider. A discovered mint is just a
name to look at: it goes through the same DexScreener pair, cross-checked
RPC reads and hard filters as a watchlist mint, so a wrong answer can only
cost a candidate, never let one through.

## API (owner session, like every `/v1` route)

- `GET /v1/trading/meme/overview`: enabled, running, paused, stake, equity,
  P&L net of fees, failed attempts and AI cost, open positions (each with its
  strategy version), gate report and goal progress, for the whole paper
  book. The top-level `strategy_version` is **deprecated** and kept for
  older clients: it is the active version of the first strategy by id
  (normally `a-survivor-momentum`) and scopes nothing else in the response.
- `GET /v1/trading/meme/candidates?limit=1..100` (default 50).
- `GET /v1/trading/meme/trades?limit=1..100`: closed paper trades.
- `POST /v1/trading/meme/pause`: stops entries at once and persists the pause
  (security audit `memecoin.paper.pause`). Exits continue. A `503` means the
  pause holds only until the next restart; retry until it answers `200`.

## Resuming

There is no resume over HTTP. To clear a pause, copy `paused_at` from the
overview into `JARVIS_MEMECOIN_PAPER_RESUME_PAUSED_AT` in `core.env` and
restart Core. Only the pause made at exactly that time is cleared, so the
setting can never undo a later pause; remove the line afterwards.
