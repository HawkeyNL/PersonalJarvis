# Docker en deployment

## Start-Compose

```yaml
services:
  api:
    build: ../../services/api
    restart: unless-stopped
    depends_on: [postgres]
    env_file: .env.api
    networks: [internal, edge]

  worker:
    build: ../../services/orchestrator
    restart: unless-stopped
    depends_on: [postgres]
    env_file: .env.worker
    networks: [internal]

  broker_gateway:
    build: ../../services/broker-gateway
    restart: unless-stopped
    depends_on: [postgres]
    env_file: .env.broker
    networks: [internal, broker_tunnel]

  postgres:
    image: postgres:17
    restart: unless-stopped
    volumes:
      - postgres_data:/var/lib/postgresql/data
    networks: [internal]

  reverse_proxy:
    image: caddy:2
    restart: unless-stopped
    ports: ["80:80", "443:443"]
    volumes:
      - ./Caddyfile:/etc/caddy/Caddyfile:ro
      - caddy_data:/data
    networks: [edge]

networks:
  edge:
  internal:
    internal: true
  broker_tunnel:
    internal: true

volumes:
  postgres_data:
  caddy_data:
```

## Productieadvies

- images pinnen op digest;
- non-root containers;
- read-only filesystem waar mogelijk;
- healthchecks;
- resource limits;
- secrets niet in image;
- SBOM;
- dependency/container scanning;
- automatische deploy alleen naar staging;
- handmatige promotie naar productie;
- database migrations gecontroleerd;
- backups vóór riskante deploy.

## MT5

MT5 draait op Windows, niet in dezelfde Linux Compose. Verbind via VPN/tunnel met een kleine proxy/service. Expose geen onbeveiligde MCP-poort op internet.
