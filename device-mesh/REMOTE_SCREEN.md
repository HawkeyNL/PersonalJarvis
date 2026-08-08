# Remote Screen Control

## Goal

Allow the user to view or control a trusted laptop or desktop when it is online.

## Separation of concerns

Remote screen software is separate from the Jarvis Device Agent.

```text
Jarvis
→ requests approved support session
→ Device Agent starts/authorizes session
→ remote desktop channel
```

Jarvis does not receive unrestricted continuous screen access.

## Options

- OS-native remote desktop;
- RustDesk with self-hosted rendezvous/relay;
- MeshCentral for managed devices;
- RDP for Windows over VPN;
- screen sharing on macOS;
- VNC only over a secure private network.

## Security rules

- device must be trusted;
- user must explicitly approve session or configure a narrow unattended policy;
- visible session indicator;
- session timeout;
- clipboard/file transfer separately permissioned;
- no public RDP/VNC ports;
- session start/stop audited;
- screenshots are not stored unless explicitly requested;
- financial/broker UI still requires separate approval.

## Home Node itself

The Home Node normally has no GUI. Use SSH and Cockpit. Graphical remote desktop is optional and not required for daily operation.
