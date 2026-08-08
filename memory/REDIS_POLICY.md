# Redis Policy

## Status

Optioneel in de eerste versie.

## Gebruik Redis alleen voor tijdelijke data

- short-lived working memory;
- locks;
- rate limits;
- queues;
- ephemeral sessions;
- websocket presence;
- korte caches;
- distributed job coordination.

## Niet gebruiken als centrale waarheid

Als Redis volledig leeg raakt, moet Jarvis kunnen herstellen uit PostgreSQL.

## Voorbeelden

```text
working_memory:{agent_run_id}
TTL: 30 minutes

rate_limit:{device_id}
TTL: sliding window

lock:broker_sync:{account_id}
TTL: short, renewable
```

## Wanneer Redis toevoegen

Pas wanneer één van deze problemen echt bestaat:

- meerdere workers hebben locks nodig;
- PostgreSQL queueing wordt onhandig;
- realtime sessions moeten gedeeld worden;
- caching levert aantoonbare winst;
- rate limiting moet distributed.

Voor één gebruiker kan PostgreSQL + procesgeheugen lang voldoende zijn.

## Security

- niet publiek exposen;
- private Docker network;
- authenticatie;
- TLS wanneer buiten localhost/private network;
- geen secrets als plain values;
- korte TTL's;
- memory limits en eviction policy.
