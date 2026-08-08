# Markt- en nieuwsproviders

## Aanpak

Start met één betaalbare provider en officiële bedrijfsbronnen. Voeg geen vier overlappende API's toe voordat gaten aantoonbaar zijn.

## Provideradapter

```rust
pub trait MarketDataProvider {
    async fn quote(&self, instrument: InstrumentId) -> Result<Quote>;
    async fn candles(&self, query: CandleQuery) -> Result<Vec<Candle>>;
    async fn corporate_actions(&self, instrument: InstrumentId) -> Result<Vec<CorporateAction>>;
}

pub trait NewsProvider {
    async fn search(&self, query: NewsQuery) -> Result<Vec<NewsItem>>;
}
```

## Officiële bronnen

- company investor relations;
- exchange/regulatory filings;
- ETF issuer factsheets;
- broker account/execution state.

## Licentiecheck

Leg per provider vast:

- persoonlijk/commercieel gebruik;
- opslagtermijn;
- caching;
- redistributie;
- realtime/delayed;
- attribution;
- request limits.

De app is persoonlijk, maar dat maakt scraping of redistributie niet automatisch toegestaan.
