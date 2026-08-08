# Headless management

## Primary management: SSH

Install Ubuntu Server LTS and OpenSSH.

Recommended controls:

- key-only authentication;
- password login disabled;
- root login disabled;
- separate admin user;
- sudo with audit logging;
- firewall allows SSH only from LAN/VPN;
- fail2ban optional;
- SSH host keys backed up/fingerprinted;
- no SSH port exposed directly to the public internet.

Preferred access paths:

```text
Laptop/desktop
→ local LAN
→ Home Node SSH
```

or remotely:

```text
Laptop/iPhone
→ Tailscale/WireGuard
→ Home Node SSH
```

## Web management

Optional Cockpit can provide:

- CPU/RAM/disk;
- service status;
- logs;
- software updates;
- terminal;
- storage/network views.

Cockpit must only be reachable over LAN/VPN and requires strong authentication.

## Graphical desktop

A full desktop environment is not recommended for the Home Node because it:

- consumes more resources;
- increases attack surface;
- adds update complexity;
- is usually unnecessary.

If a GUI is genuinely needed:

- install a lightweight desktop;
- use xrdp or a controlled remote-desktop solution;
- require VPN;
- do not expose RDP/VNC publicly;
- treat the graphical session as an administrative exception.

## Recovery access

Keep a USB-C/HDMI adapter or temporary monitor option available for:

- boot failure;
- broken network configuration;
- disk encryption unlock/recovery;
- BIOS/firmware work.

Optional later:

- remote KVM device;
- Intel vPro/AMT model where available and securely configured.
