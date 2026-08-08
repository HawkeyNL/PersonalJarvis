# Client Cache

## Doel

Snelle UI en beperkte offline leesmodus zonder de centrale waarheid naar elk apparaat te kopiëren.

## Opslag

Elke Tauri-client gebruikt een versleutelde SQLite-cache.

Voorbeelden:

- laatste gesprekken;
- recente summaries;
- dashboards;
- notificaties;
- settings;
- read-only portfolio snapshot;
- sync cursor.

## Niet lokaal opslaan

- OpenAI/Claude API-keys;
- brokercredentials;
- MT5-credentials;
- private wallet keys buiten een apart strikt walletdesign;
- volledige centrale memory;
- server-side master encryption keys.

## Secure storage

Gebruik OS secure storage voor:

- refresh token;
- device private key;
- lokale database-encryptiesleutel.

## Sync

```text
1. toon lokale cache
2. stuur sync cursor
3. backend retourneert changes
4. transactioneel toepassen
5. nieuwe cursor bewaren
```

## Offline gedrag

- read-only toegestaan;
- geen financiële submit queue;
- geen background LLM op telefoon;
- mutaties wachten tot online en worden opnieuw gevalideerd.
