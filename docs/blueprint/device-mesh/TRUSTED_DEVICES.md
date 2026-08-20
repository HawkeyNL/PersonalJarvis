# Trusted Devices

## Enrollment

1. User starts enrollment from an already trusted device.
2. New device generates its own keypair.
3. Jarvis displays a short code/QR challenge.
4. User confirms.
5. Backend issues a device certificate with narrow scopes.
6. Device appears in the Observatory.

## Device states

- pending;
- trusted;
- restricted;
- quarantined;
- revoked;
- offline;
- compromised suspected.

## Stored metadata

- device ID;
- public key/certificate;
- platform;
- owner;
- capabilities;
- last seen;
- software version;
- risk score;
- certificate expiry;
- revoke history.

Private device keys remain on the device.
