# Interactive Brokers-integratie

## Aanbeveling

Bouw IBKR als een **interne `BrokerAdapter`**, niet als de primaire MCP-laag.

## Officiële opties

### Client Portal Web API

- HTTP + WebSocket.
- Marktdata, scanners, portfolio-updates en tradingfunctionaliteit.
- Past conceptueel goed bij een backend.
- Authenticatie- en sessiegedrag voor individuele accounts moet praktisch getest worden.
- Houd rekening met re-authenticatie en paper/live scheiding.

### TWS API via TWS of IB Gateway

- TCP socket protocol.
- Officiële libraries voor onder andere Python, Java, C++, C# en Visual Basic.
- Andere talen kunnen het protocol implementeren of een bridge gebruiken.
- Zeer uitgebreid, maar operationeel afhankelijk van TWS/IB Gateway.

## Keuze voor dit project

### Start

- IB Gateway of Client Portal in paperomgeving.
- Een aparte `ibkr-connector` service.
- Rust domeinmodel, desnoods kleine Java/Python sidecar wanneer officiële SDK-stabiliteit zwaarder weegt dan pure Rust.

### Niet doen

Geen ongeteste community Rust-library direct verantwoordelijk maken voor live geld. Zet altijd contracttests en reconciliation rond de adapter.

## BrokerAdapter-interface

```rust
#[async_trait]
pub trait BrokerAdapter {
    async fn health(&self) -> Result<BrokerHealth>;
    async fn accounts(&self) -> Result<Vec<BrokerAccount>>;
    async fn balances(&self, account: &AccountId) -> Result<Balances>;
    async fn positions(&self, account: &AccountId) -> Result<Vec<Position>>;
    async fn open_orders(&self, account: &AccountId) -> Result<Vec<Order>>;
    async fn preview(&self, proposal: &ValidatedProposal) -> Result<OrderPreview>;
    async fn submit(&self, command: &ApprovedOrderCommand) -> Result<Submission>;
    async fn cancel(&self, command: &ApprovedCancelCommand) -> Result<CancelResult>;
    async fn executions(&self, since: Timestamp) -> Result<Vec<Execution>>;
}
```

## IBKR-specifieke aandachtspunten

- contract resolution (`conid`) centraal cachen;
- exchange/currency duidelijk;
- fractional availability per instrument;
- order precautions/warnings afhandelen;
- partial fills;
- parent/child/bracket orders;
- market data subscriptions;
- delayed versus realtime;
- pacing/rate limits;
- reconnect en next-valid-order-ID waar toepasselijk;
- paper en live account nooit verwarren;
- MiFIR/regionale velden waar vereist;
- broker “what-if”/preview benutten;
- brokerstate periodiek reconciliëren.

## Eerste milestones

1. Health/session.
2. Accounts.
3. Cash en positions.
4. Transactions/executions.
5. Open orders.
6. Contract search.
7. Paper preview.
8. Paper submit met handmatige approval.
9. Cancel.
10. Reconciliation.
11. Pas veel later assisted live.
