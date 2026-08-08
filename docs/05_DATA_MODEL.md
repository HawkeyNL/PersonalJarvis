# Datamodel

## Identiteit en apparaten

- `users`
- `devices`
- `device_keys`
- `sessions`
- `approvals`
- `api_credentials_metadata`

## Agents en gesprekken

- `conversations`
- `messages`
- `agent_runs`
- `agent_steps`
- `tool_calls`
- `model_usage`
- `memories`
- `prompt_versions`

## Markt en research

- `instruments`
- `venues`
- `quotes`
- `candles`
- `corporate_actions`
- `economic_events`
- `news_articles`
- `news_entities`
- `filings`
- `documents`
- `document_chunks`
- `research_notes`
- `market_snapshots`

## Portfolio en brokers

- `broker_accounts`
- `broker_connections`
- `portfolio_snapshots`
- `positions`
- `cash_balances`
- `transactions`
- `order_proposals`
- `orders`
- `executions`
- `fees`
- `reconciliation_runs`
- `allocation_targets`

## Tradingstrategieën

- `strategies`
- `strategy_versions`
- `strategy_parameters`
- `signals`
- `risk_profiles`
- `risk_decisions`
- `backtest_runs`
- `backtest_trades`
- `backtest_metrics`
- `walk_forward_runs`
- `paper_deployments`
- `live_deployments`

## Content

- `content_sources`
- `trend_items`
- `content_ideas`
- `scripts`
- `assets`
- `render_jobs`
- `publications`
- `content_metrics`
- `content_experiments`

## Operations

- `jobs`
- `job_runs`
- `notifications`
- `audit_events`
- `outbox_events`
- `feature_flags`
- `kill_switches`

## Belangrijke invarianten

- Brokerorder-ID uniek per brokeraccount.
- `order_proposals` zijn immutable na approval.
- Geldbedragen als decimal/numeric, nooit float.
- Tijden in UTC; bronzone apart bewaren.
- Candles hebben provider, venue, timeframe en adjustment-status.
- Nieuws bewaart zowel `published_at` als `event_at` indien bekend.
- Backtest bewaart code/config/data-versies.
- Iedere agentrun bewaart model, promptversie en toolversies.
