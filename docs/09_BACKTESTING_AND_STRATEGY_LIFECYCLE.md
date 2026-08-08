# Backtesting en strategielevenscyclus

## Doel

Een strategie wordt als code en versie beheerd. De AI mag hypotheses en wijzigingen voorstellen, maar resultaten komen uit een reproduceerbare backtest.

## Pipeline

```text
idee
→ formele strategie-specificatie
→ implementatie
→ unit tests
→ historische backtest
→ kosten/slippage stress
→ out-of-sample
→ walk-forward
→ Monte Carlo / trade reshuffling
→ paper/demo
→ shadow monitoring
→ beperkte live pilot
→ periodieke review
```

## Verplichte metrics

- total return
- CAGR waar passend
- maximum drawdown
- profit factor
- expectancy
- Sharpe/Sortino met duidelijke aannames
- win rate
- average win/loss
- exposure time
- turnover
- commissions
- spread/slippage impact
- consecutive losses
- tail loss
- metrics per jaar, sessie, instrument en regime

## Valkuilen

- look-ahead bias
- survivorship bias
- data snooping
- overfitting
- cherry-picked periode
- ontbrekende kosten
- onrealistische fills
- timezone fouten
- future-known revised macro data
- meerdere tests zonder correctie
- strategie wijzigen op basis van testset

## MT5

Gebruik MT5 Strategy Tester voor MQL5-strategieën. Exporteer:

- strategy code hash;
- terminal/build;
- broker/symbol settings;
- tick model;
- spread;
- data range;
- parameters;
- report;
- trades.

Jarvis analyseert de output, maar herschrijft een strategie niet automatisch naar live.

## Promotion gates

Een strategie kan alleen promoveren als:

- tests slagen;
- minimum aantal trades is gehaald;
- OOS-resultaat acceptabel is;
- drawdown binnen limiet valt;
- paperperiode is voltooid;
- geen ongekende execution errors bestaan;
- gebruiker expliciet akkoord geeft.
