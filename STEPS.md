
## Stap 0B — engineering agent framework

1. Engineering Orchestrator.
2. Architecture Research Agent.
3. Codebase Impact Agent.
4. ADR/design templates.
5. Independent reviewers.
6. Fix/reverification loop.
7. Engineering Memory.
8. Structured logs/traces/metrics.
9. Observability Intelligence Agent.
10. Incident Learning Agent.

Geen medium/high-impact wijziging gaat rechtstreeks van verzoek naar code.

# Stappenplan

## 1. Werkende lege applicatie
Rust workspace, Tauri, Axum, SurrealDB, Docker, CI en logging.

## 2. Identity en sync
Users, devices, sessions, keychain, SQLite-cache en cursor-sync.

## 3. Agent runtime zonder financiële writes
Providertrait, cloudmodel, Ollama, structured outputs, tools, audit en kostenregistratie.

## 4. Marktdata, nieuws en portfolio
Instrument master, quotes, candles, nieuws, portfolio, allocator en rapporten.

## 5. IBKR paper read-only
Account, cash, positions, orders, executions en reconciliation.

## 6. Futuresdata
Contractmetadata, sessies, trades, depth, orderbook, features en replay.

## 7. NautilusTrader beslissing
Pinned PoC, één futuresbacktest, replay, IBKR-adapteronderzoek, performancevergelijking en ADR.

## 8. Risk engine
Money/tick math, 0,5–1% default, harde limieten, daily/weekly stop, drawdown, correlation en property tests.

## 9. Paper execution
Immutable proposal, approval, biometrie, fresh-data validation, submit, fills, cancel en reconciliation.

## 10. MT5 en prop companion
MCP inventory, read-only proxy, prop policies, drawdown en cutoff alerts.

## 11. Backtesting en strategy lifecycle
Specs, versions, costs, OOS, walk-forward, Monte Carlo, journal en promotion gates.

## 12. Crypto of prediction markets
Kies één experiment. Eerst data, dan shadow, dan paper. Niet tegelijk beide bouwen.

## 13. Economics engine
Alle AI-, infra-, data-, broker- en propkosten koppelen aan agents en strategieën.

## 14. Content engine
Trends, scripts, assets, render, review en analytics.

## 15. Assisted live
Pas na paperbewijs, kleine limieten, handmatige approval en geteste kill switch.
