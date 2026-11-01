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
2. Put the market-data settings in `/etc/jarvis/core.env` (root:jarvis 0640):
   - `JARVIS_MARKET_SOLANA_RPC_A_URL` and `JARVIS_MARKET_SOLANA_RPC_B_URL`:
     two HTTPS Solana RPC endpoints on **different** hosts from the
     market-data allowlist (`api.mainnet-beta.solana.com`,
     `mainnet.helius-rpc.com`, `solana-mainnet.g.alchemy.com`). Every read is
     made on both and must agree. The URLs usually contain a provider key;
     Core redacts them in its logs.
   - `JARVIS_MARKET_JUPITER_API_KEY` (optional): without it, quotes are
     paced to one per 2.1 s.
   - `JARVIS_MEMECOIN_PAPER_ENABLED=true`.
3. Restart Core. The log shows `memecoin PAPER trader running` or the reason
   it stayed off.

`jarvis-credentials` does not manage these three secrets yet; adding them as
providers (`solana-rpc-a`, `solana-rpc-b`, `jupiter`, read from the TTY with a
read-only probe and their own `EnvironmentFile`) is a follow-up.

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
  `failure_rate` (fraction).

Everything except `watchlist` and `goal_multiple` is recorded with the strategy
version. Changing any of it without raising `strategy.version` keeps the
trader off. Reports and the paper gate cover the current version only; open
positions of an older version are still exited.

## What a tick does

Every 60 s: reload the paper ledger from the database and check it replays
exactly; value open positions and simulate exits (also while paused); unless
paused, observe up to 5 watchlist mints (each at most every 15 minutes,
through the per-provider pacing of `jarvis-market-data`), run the filter and
the strategy, and simulate entries. A write failure stops the tick before
anything is simulated on it. A candidate row is written only when a mint's
verdict changes. Candidate rows and the hourly gate snapshots are pruned after
30 days; trips and costs are kept for the paper gate.

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
  (security audit `memecoin.paper.pause`). Exits continue.

## Resuming

There is no resume over HTTP. To clear a pause, copy `paused_at` from the
overview into `JARVIS_MEMECOIN_PAPER_RESUME_PAUSED_AT` in `core.env` and
restart Core. Only the pause made at exactly that time is cleared, so the
setting can never undo a later pause; remove the line afterwards.
