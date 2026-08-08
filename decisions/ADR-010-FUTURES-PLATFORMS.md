# ADR-010 — Futures platform strategy

## Status

Proposed.

## Decision

1. IBKR is the strategic own-capital broker.
2. Prop firms get adapters only after choosing one specific firm/plan.
3. Topstep uses TopstepX integration if selected.
4. MFFU uses the actual supported platform for the selected plan.
5. NinjaTrader/Tradovate integrations require an API-access spike.
6. MT5 remains a separate CFD/MT5 domain, not the source of truth for CME futures.

## Rationale

This avoids building against an assumed platform that a prop firm no longer supports and prevents prop rules from contaminating the core broker model.
