# Jarvis Home Node

## Physical setup

The Home Node is installed headless in the meter cupboard or network cabinet.

Required connections:

- power;
- wired Ethernet.

No permanent:

- monitor;
- keyboard;
- mouse;
- Wi-Fi dependency.

## Services

- `jarvis-home-agent`;
- outbound mTLS/VPN tunnel;
- device presence and health collector;
- local DNS/service discovery;
- task relay;
- Wake-on-LAN coordinator;
- optional Home Assistant;
- optional MQTT;
- optional Cockpit web console;
- optional remote-support relay.

## Responsibilities

- bridge between VPS and LAN;
- keep device state available even when personal computers are off;
- execute local network tasks with narrow capabilities;
- report temperature, disk, load, services and tunnel health;
- maintain a small encrypted local queue/cache.

## Not responsible for

- large LLM inference;
- primary PostgreSQL database;
- unrestricted remote shell orchestration;
- direct public exposure;
- latency-critical market execution.

## Failure behaviour

When the Home Node is offline:

- VPS and mobile apps remain operational;
- cloud agents keep working;
- local device tasks are unavailable;
- Jarvis raises an alert;
- no sensitive task is automatically replayed after reconnect.
