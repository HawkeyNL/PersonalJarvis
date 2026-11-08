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
- `strategy`: `version` and the `StrategyParams` fields of `jarvis-onchain`
  (`*_secs` for durations); validated against the code-fixed bounds.
- `costs`: `dex_fee_bps`, `network_fee_usd`, `priority_fee_usd`,
  `failure_rate` (fraction, below 1).

Everything except `watchlist`, `discovery` and `goal_multiple` is recorded
with the strategy version. Changing any of it without raising
`strategy.version` keeps the trader off; that includes rewriting `"25"` as
`"25.0"`, since decimals keep their scale. Reports and the paper gate cover
the current version only; open positions of an older version are still
exited.

## What a tick does

Every 60 s: value held positions and simulate exits (also while paused);
then, if the stored ledger replays exactly and the trader is not paused,
poll discovery (at most every 2 minutes), observe up to 5 mints (watchlist
first, then discovered mints; each at most every 15 minutes, through the
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
- State is in memory only. After a restart discovery starts from the newest
  200 signatures; nothing is stored until a mint becomes a candidate row.

Discovery reads only the primary RPC provider. A discovered mint is just a
name to look at: it goes through the same DexScreener pair, cross-checked
RPC reads and hard filters as a watchlist mint, so a wrong answer can only
cost a candidate, never let one through.

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
