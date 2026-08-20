# Device Agent

## Platforms

- Linux;
- Windows;
- macOS;
- Windows VPS;
- later other systems.

## Connection model

Each Device Agent initiates an outbound authenticated connection to the Home Node or VPS. No inbound public port is required.

## Health telemetry

- online/offline;
- CPU/RAM/GPU;
- disk;
- temperature where available;
- battery;
- network;
- Docker/services;
- agent version;
- last successful task.

## Commands

Only typed capabilities, for example:

- `system.health.read`;
- `service.status`;
- `service.restart`;
- `docker.list`;
- `docker.logs.read`;
- `docker.restart`;
- `git.status`;
- `application.launch`;
- `file.read.scoped`;
- `screen.session.request`.

No generic unrestricted shell by default.
