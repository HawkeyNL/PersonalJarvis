# Device Mesh Security

- outbound connections only where possible;
- private WireGuard/Tailscale network;
- mTLS/device certificates;
- per-device capabilities;
- certificate rotation and revocation;
- signed, expiring task commands;
- anti-replay nonces;
- no shared SSH private key across devices;
- SSH public keys per administrator;
- no public SSH/RDP/VNC/Cockpit;
- unattended upgrades with staged policy;
- encrypted disk where practical;
- Secure Boot/TPM where supported;
- audit every administrative action;
- quarantine outdated or anomalous agents.
