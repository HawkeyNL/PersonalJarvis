# Device Mesh Implementation Plan

## D1 — Home Node bootstrap

- install Ubuntu Server LTS;
- static DHCP reservation;
- hostname and DNS;
- SSH keys;
- firewall;
- automatic security updates;
- VPN/tunnel;
- monitoring;
- backup configuration.

## D2 — Device identity

- keypair generation;
- enrollment;
- certificates;
- registry;
- revoke/quarantine.

## D3 — Presence and health

- heartbeat;
- health metrics;
- offline detection;
- Observatory nodes and alerts.

## D4 — Typed capabilities

- read-only first;
- service/Docker controls;
- approvals;
- signed expiring tasks;
- audit.

## D5 — Remote screen

- select platform;
- VPN-only;
- session approval;
- visible indicator;
- timeout and audit.

## D6 — Home automation

- optional MQTT/Home Assistant;
- isolated permissions;
- no broad LAN access for AI agents.

## Definition of Done

- Home Node works with only power and Ethernet;
- SSH works over LAN and VPN;
- node recovers after power loss;
- trusted laptop appears online/offline;
- one low-risk remote task succeeds;
- one remote-screen session requires approval;
- no management port is public;
- Observatory shows node/server health.
