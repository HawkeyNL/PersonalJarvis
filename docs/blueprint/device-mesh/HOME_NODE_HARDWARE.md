# Home Node Hardware Recommendation

## Workload

The Home Node needs:

- 24/7 reliability;
- low idle power;
- wired networking;
- Ubuntu Server;
- Docker containers;
- VPN/tunnel;
- Device Mesh;
- Home Assistant/MQTT optionally;
- PostgreSQL replica/cache optionally;
- no large local LLM.

## Recommended main option

### ASUS NUC 14 Pro — Core 3 100U or Core Ultra 5, Tall chassis

Suggested configuration:

- Core 3 100U when efficiency and cost are primary;
- Core Ultra 5 when more containers, development or future local services are expected;
- 32 GB DDR5;
- 1 TB reliable NVMe;
- 2.5 GbE;
- Tall chassis for storage/thermal flexibility;
- Ubuntu Server LTS.

Why:

- established NUC platform;
- replaceable memory/storage;
- official 2.5 Gb Ethernet;
- broad I/O;
- sufficiently powerful without becoming an AI workstation;
- easier long-term servicing than many sealed mini PCs.

## Budget/ultra-efficient option

### Intel N100/N150 mini PC

Suggested:

- 16–32 GB RAM;
- 512 GB–1 TB NVMe;
- preferably 2.5 GbE;
- reputable vendor and replaceable SSD;
- verify Linux NIC support.

Choose this when the node only runs networking, device agents, Home Assistant, monitoring and light Docker services.

## Avoid overbuying

Do not buy:

- Jetson Thor;
- DGX Spark;
- discrete-GPU mini PC;
- Core Ultra 9;
- gaming NUC;

for the Home Node role alone.

Those belong to a future separate AI node.

## Reliability checklist

- wired Ethernet;
- BIOS option: power on after AC loss;
- Wake-on-LAN;
- TPM/Secure Boot;
- NVMe health monitoring;
- fan/ventilation suitable for meter cupboard;
- UPS optional;
- temperature sensor/alerts;
- vendor BIOS updates;
- spare backup boot USB;
- confirm Ubuntu installation before permanent placement.

## Meter cupboard warning

Measure ambient temperature first. Keep air space around the unit and do not place it against hot electrical equipment.
