# Jarvis for iOS

Native SwiftUI client for iPhone and iPad. The Home Node remains authoritative;
this target contains no Jarvis intelligence or provider credentials.

## Requirements

- macOS with Xcode 16 or newer
- iOS 17 simulator, or an iOS/iPadOS 17+ device
- Optional: XcodeGen 2.42+ when changing `project.yml`

Open `Jarvis.xcodeproj`, select the `Jarvis` scheme and an iPhone or iPad
simulator, then Build or Run. The bundle identifier is
`com.hawkeynl.jarvis`. Simulator builds do not require production signing.
For a physical device, select your own development team in Xcode; do not commit
provisioning profiles, certificates, team IDs, or credentials.

In Simulator, enroll Face ID/Touch ID from the simulator Features menu before
testing the app lock. A real device must have biometrics or a device passcode
configured. Use a Home Node certificate trusted by the device for HTTPS. Local
`http` is intended only for LAN development; ATS is not globally disabled.

## TestFlight releases

Jarvis iOS uses Apple's native signing and App Store Connect/TestFlight. It has
no Home Node self-updater. The protected release workflow imports an ephemeral
distribution certificate and provisioning profile, archives with the requested
marketing/build versions, uploads with an App Store Connect API key and removes
the signing material afterward. Certificates, profiles and API credentials are
never stored in this repository.

See [`docs/app-updates/PRIVATE_RELEASES.md`](../docs/app-updates/PRIVATE_RELEASES.md)
for the complete private release boundary.

Command-line simulator validation on macOS:

```sh
xcodebuild -project Jarvis.xcodeproj \
  -scheme Jarvis \
  -sdk iphonesimulator \
  -destination 'platform=iOS Simulator,name=iPhone 16' \
  CODE_SIGNING_ALLOWED=NO build

xcodebuild -project Jarvis.xcodeproj \
  -scheme Jarvis \
  -sdk iphonesimulator \
  -destination 'platform=iOS Simulator,name=iPhone 16' \
  CODE_SIGNING_ALLOWED=NO test
```

After editing `project.yml`, regenerate the checked-in project with:

```sh
xcodegen generate
```

## Milestone 1 behavior

Settings accepts an `http` or `https` Home Node origin with no embedded
credentials or API path. Jarvis checks `/readyz`, presents unreachable and
timeout states, then uses the production pairing flow:

1. Generate a unique Ed25519 identity on the iPhone/iPad.
2. `POST /v1/auth/pairing/requests` with the public key.
3. Poll with the pairing nonce in `X-Jarvis-Pairing-Nonce` until an existing
   trusted device approves the candidate public key.
4. Sign the raw login challenge natively and store the resulting session.

The Ed25519 seed, pending-pairing nonce, registered device ID, and bearer token
are separate, non-synchronizing `WhenUnlockedThisDeviceOnly` Keychain records.
The private key is never returned by `DeviceIdentityStore`. Logout removes the
session but preserves the device identity; Reset attempts server revocation and
then removes all local identity/auth material so re-enrollment creates a new
key.

Networking uses an ephemeral `URLSession` with no URL cache, cookie store, or
credential store. Bearer tokens are sent only in the `Authorization` header and
are never included in URLs or error descriptions.

CryptoKit Ed25519 keys are used for compatibility with Jarvis's current wire
protocol. Secure Enclave keys cannot currently represent Ed25519 keys, so using
the enclave would silently change the protocol and is intentionally deferred.
App unlock uses `LocalAuthentication` with Apple's device-owner authentication
policy (biometrics with system device-credential fallback).

Local network access and Face ID usage descriptions are present. Milestone 1
does not request microphone, notification, push, or background-audio
permissions. The privacy manifest declares no tracking or collected-data SDK
behavior and records the app-owned `UserDefaults` use for endpoint metadata.
Voice is deliberately a placeholder until native milestone 2.

## Known contract boundary

The backend currently exposes route-private Rust request types and hand-built
JSON responses; it does not publish OpenAPI or generated Swift models. DTOs in
`Jarvis/Domain/ClientDTOs.swift` conservatively mirror the inspected v1 routes
and the current Rust-only `jarvis-client-core` shapes. They should be replaced
by generated bindings/schema once that crate exposes a stable mobile boundary.
Response optionality and a protocol compatibility/version endpoint still need
authoritative schemas before independent server/client evolution.
