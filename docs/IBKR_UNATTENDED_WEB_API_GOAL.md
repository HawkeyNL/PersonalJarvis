# Goal: unattended IBKR Web API trading integration

Implement the PersonalJarvis IBKR integration around the Trading Web API with unattended OAuth authentication, rather than relying on the Client Portal Gateway's daily interactive login.

## Authentication

- Inspect the current IBKR adapter and current IBKR Web API documentation before implementation.
- Prefer the authentication flow that genuinely supports unattended Home Node operation. Evaluate OAuth 2.0 against OAuth 1.0a before choosing; do not choose OAuth 1.0a merely because it was previously planned.
- If OAuth 1.0a is selected, implement the complete lifecycle: consumer registration inputs, one-time user authorization/access-token acquisition, protected Access Token + Access Token Secret storage, Live Session Token derivation, mandatory LST signature validation, expiry tracking/renewal, and brokerage-session initialization/reinitialization.
- Never log RSA private keys, Access Token Secret, LST, OAuth signatures, DH private values, session credentials, or order-sensitive secrets.
- Automatically recover from LST expiry, inactive brokerage sessions and normal IBKR resets where the API permits this without interactive login.
- Detect and surface IBKR's one-brokerage-session-per-username limitation rather than fighting another TWS/Client Portal session.

## Cash-quantity stock orders

Extend the order model with explicit typed sizing modes:

- share quantity (`quantity`)
- money amount (`cashQty`)

Jarvis must be able to express requests such as "buy EUR/USD 100 worth of this stock" without first approximating a fractional share quantity when the selected contract supports native cash-quantity orders.

Before a cash-quantity order:

1. Resolve and pin the exact IBKR contract/conid, listing and currency.
2. Query contract/order rules and verify cash-quantity/fractional support (`cqtTypes` or the current documented equivalent).
3. Validate account permissions, selected account, currency/cash availability and order constraints.
4. Build the order with `cashQty`, not `quantity`.
5. Preserve Jarvis risk/policy/device-signed approval boundaries.
6. Handle IBKR order replies/confirmations safely; never auto-confirm owner-relevant warnings without an explicitly reviewed policy.

## Session lifecycle

Target:

```text
Home Node starts
  -> load protected OAuth credentials
  -> establish/renew verified Live Session Token
  -> initialize brokerage session
  -> expose IBKR adapter as ready
  -> monitor session state/expiry
  -> renew/reinitialize automatically when allowed
```

Normal IBKR resets or LST expiry must degrade to a clear reconnecting/unavailable state rather than causing unsafe retries or duplicate orders.

## Duplicate-order safety

Persist enough local order intent/correlation state to reconcile ambiguous submissions against IBKR after network timeout, process restart or session renewal. Never blindly resubmit because an HTTP response was lost.

Start with paper trading. Live trading remains gated until explicit owner enablement after real-machine verification.

## Boundaries

- Credentials/signing remain server-side on the Home Node.
- Do not expose raw IBKR credentials or a generic arbitrary IBKR proxy through the public Jarvis API.
- Jarvis Core uses a typed broker adapter/capability.
- Trading actions flow through policy, risk, approval and audit.
- Read-only account/position functionality should continue independently where possible.

## Tests

Cover at minimum:

- OAuth signing/canonicalization with fixed vectors
- LST derivation and mandatory signature verification
- invalid LST signature rejection
- expiry/renewal
- brokerage-session reinitialization
- concurrent refresh locking
- secret redaction
- another active brokerage session
- share-quantity orders
- cashQty orders
- unsupported cashQty rejection
- contract/currency mismatch rejection
- warning/reply handling
- ambiguous timeout reconciliation
- duplicate-order prevention
- paper/live gating
- invalid/revoked credentials fail closed

Normal CI must use mocks/fixtures and require no live IBKR credentials.

## Documentation

Document the selected authentication mode and why, initial IBKR setup/consumer registration, required credentials, secure Home Node installation, automatic session lifecycle, the single-brokerage-session limitation, reset behavior, paper verification, `cashQty` vs `quantity`, failure states, and credential recovery/revocation.

## Definition of done

PersonalJarvis can maintain the supported IBKR Web API authentication/session lifecycle on the Home Node without a daily interactive Client Portal Gateway login, safely recover from normal session resets, and create native cash-quantity stock orders for supported contracts while preserving Jarvis risk, signed approval, audit, paper/live gating and duplicate-order protections.

Do not enable autonomous live trading as part of this milestone.