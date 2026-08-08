# Risk Manager — deterministische service

> Dit is primair gewone Rust-code, niet een vrije LLM-agent.

## Doel

Geeft `ALLOW`, `DENY` of `REQUIRE_REVIEW`.

## Inputs

- account equity/cash/margin
- current exposure
- proposal
- fresh quote/spread
- risk profile
- daily P&L/drawdown
- broker constraints
- active kill switches

## Checks

- max risk per trade
- max daily/weekly loss
- max notional/leverage
- max correlated exposure
- max positions
- stop distance
- size rounding
- margin
- spread/slippage
- trading window
- event blackout
- stale data
- duplicate order
- mode/account

## Output

```json
{
  "decision": "ALLOW",
  "approved_quantity": "0.01",
  "max_loss": "5.00",
  "currency": "EUR",
  "reasons": [],
  "expires_at": "..."
}
```

## Regel

Geen enkele agent kan deze beslissing overrulen.
