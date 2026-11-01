# Memecoin paper trader

Core can run the on-chain memecoin strategy as a **paper** simulation: it reads
public market data, applies the hard filters and the strategy from
`jarvis-onchain`, and simulates fills, fees and failed attempts. It has no
wallet, no signer and no execution path, and no LLM output reaches it. Results
are stored in schema 0014 and shown through `/v1/trading/meme/*`.

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

- `watchlist`: 1–20 distinct base58 mints to observe (no discovery yet).
- `goal_multiple`: target multiple of the stake (above 1), for progress only.
- `stake_usd`, `max_position_usd`, `max_open_positions` (1–10): simulation
  inputs only, never limits for live trading. The stake must cover
  `max_position_usd × max_open_positions`.
- `filter`: `min_liquidity_usd`, `min_pool_age_secs`, `max_top_holders_share`
  (fraction), `max_sell_impact_bps`, `allowed_extensions` (snake_case
  Token-2022 extension names), `max_snapshot_age_secs`,
  `min_lp_lock_remaining_secs`.
- `strategy`: `version` and the `StrategyParams` fields of `jarvis-onchain`
  (`*_secs` for durations); validated against the code-fixed bounds.
- `costs`: `dex_fee_bps`, `network_fee_usd`, `priority_fee_usd`,
  `failure_rate` (fraction, below 1).

Everything except `watchlist` and `goal_multiple` is recorded with the strategy
version. Changing any of it without raising `strategy.version` keeps the
trader off; that includes rewriting `"25"` as `"25.0"`, since decimals keep
their scale. Reports and the paper gate cover the current version only; open
positions of an older version are still exited.

## What a tick does

Every 60 s: value held positions and simulate exits (also while paused);
then, if the stored ledger replays exactly and the trader is not paused,
observe up to 5 watchlist mints (each at most every 15 minutes, through the
per-provider pacing of `jarvis-market-data`), run the filter and the
strategy, and simulate entries; finally report from the stored ledger. A
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

A candidate row is written only when a mint's verdict changes. Candidate rows
and the hourly gate snapshots are pruned after 30 days; trips and costs are
kept for the paper gate.

**Known limit:** market data cannot yet tell whether a pool's LP is burned or
locked, so every live candidate is rejected with `lp_not_secured` until LP
reading is added to `jarvis-market-data`. The trader then only records
decisions.

## API (owner session, like every `/v1` route)

- `GET /v1/trading/meme/overview`: enabled, running, paused, stake, equity,
  P&L net of fees, failed attempts and AI cost, open positions, gate report,
  goal progress.
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
