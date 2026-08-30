# Jarvis for Android

Native Android client for Jarvis. This project intentionally does not share the
desktop Tauri/WebView runtime: it uses Kotlin, Jetpack Compose and the existing
Home Node HTTP contract.

## Milestone 1

The current client provides:

- a phone-native bottom navigation shell for chat, conversations and settings;
- a configurable Home Node endpoint with explicit timeout, DNS, TLS, refused and
  generic network failure states;
- restart-safe pairing, challenge signing and session login boundaries;
- one unique Ed25519 identity per install, with its seed encrypted by a
  non-exportable Android Keystore AES-GCM wrapping key;
- encrypted-at-rest pairing nonce, device id and bearer session token;
- a strong-biometric `BiometricPrompt` lock on launch and after backgrounding;
- conversation list/load/send calls using client-local DTOs that mirror the
  current `jarvis-api` contract;
- unit tests for endpoint policy, DTO serialization, session migration boundary,
  device signatures and enrollment transitions.

Voice, wake word, notifications, background sync and Home Node discovery are not
part of this milestone.

## Private releases

Release APK/AAB builds require the stable owner-managed keystore through the
`JARVIS_ANDROID_KEYSTORE_PATH`, `JARVIS_ANDROID_KEY_ALIAS`,
`JARVIS_ANDROID_STORE_PASSWORD` and `JARVIS_ANDROID_KEY_PASSWORD` environment
variables. `JARVIS_APP_VERSION` supplies `versionName` and
`JARVIS_ANDROID_VERSION_CODE` must increase for each release. Gradle refuses
`assembleRelease` and `bundleRelease` when signing is not configured.

The protected release workflow publishes the signed APK only to private
storage. See
[`docs/app-updates/PRIVATE_RELEASES.md`](../docs/app-updates/PRIVATE_RELEASES.md).

## Prerequisites

- Android Studio Quail 2026.1.1 or newer;
- JDK 17;
- Android SDK Platform 37.0 and Build Tools 36.0.0 or newer;
- an API 28+ emulator or physical device.

No NDK is required. No production signing credentials belong in this repository.

Create `local.properties` through Android Studio, or set `ANDROID_HOME`. Then run:

```bash
./gradlew testDebugUnitTest
./gradlew lintDebug
./gradlew assembleDebug
```

Install the debug artifact on a connected emulator/device:

```bash
./gradlew installDebug
```

The APK is written below `app/build/outputs/apk/debug/`.

## Connection policy

HTTPS endpoints are accepted for local and remote Home Nodes. Cleartext HTTP is
accepted only for loopback, RFC1918 IPv4, IPv6 local addresses, or `.local`
hostnames. Android's manifest must permit cleartext globally because the endpoint
is configured at runtime; `HomeNodeEndpoint` enforces the narrower application
policy before any request is sent. HTTPS remains strongly preferred, including
on a LAN.

The endpoint is ordinary metadata and is stored in Preferences DataStore. It is
never placed in a URL query together with credentials. Requests have bounded
timeouts and the HTTP client deliberately has no logging plugin.

## Device and session security

The Home Node protocol requires a raw 32-byte Ed25519 public key. Android
Keystore does not provide portable Ed25519 key-pair generation across API 28+,
so the client generates a 32-byte Ed25519 seed in native Kotlin and immediately
wraps it with AES-GCM. Only ciphertext is persisted in app-private preferences;
the AES key is non-exportable and stored in Android Keystore with
`setUnlockedDeviceRequired(true)`. The seed is decrypted only for public-key
derivation or signing and the temporary byte buffer is cleared afterward.

The same wrapping boundary protects bearer tokens, device ids and pending
pairing nonces. Storage decryption failures fail clearly and never create a new
identity silently. Device reset best-effort calls `DELETE /v1/devices/{id}` before
clearing the local session and identity.

The app lock accepts `BIOMETRIC_STRONG` only. Device PIN/pattern/password fallback
is not enabled implicitly; changing that requires an explicit Jarvis policy
decision. Cancellation, lockout, missing enrollment, missing hardware and a
required security update are distinct UI states. If biometrics are unavailable,
the locked screen can erase this local device and start a fresh enrollment; it
does not bypass the lock.

## Current server contract

Client-local DTOs were checked against `jarvis-api/src/routes/auth.rs` and
`jarvis-api/src/routes/chat.rs`:

| Flow | Contract |
| --- | --- |
| Pairing request | `POST /v1/auth/pairing/requests` with `name`, `platform=android`, raw-key `public_key` hex |
| Pairing status | `GET /v1/auth/pairing/requests/{id}/status`, nonce in `X-Jarvis-Pairing-Nonce` |
| Login | challenge nonce is signed directly with Ed25519; login signature is 64-byte hex |
| Session | bearer token plus Unix `expires_at`; no refresh-token endpoint exists |
| Conversations | summaries use RFC3339 `updated_at`; messages use RFC3339 `at` |
| Chat | up to the latest 20 client turns plus optional `conversation_id` |

Known gaps or mismatches are deliberately isolated:

- There is no discovery endpoint, so the user must enter the Home Node address.
- First-owner bootstrap requires a separately provisioned LAN secret. This phone
  client implements trusted-device pairing only and never persists that secret.
- The API has no refresh token. An enrolled client obtains a fresh bearer token
  by signing a new challenge.
- Server chat success and refusal payloads omit different optional routing fields;
  those client fields are nullable, while conversation id/title and `new_topic`
  remain required.
- The server has no Android-specific push, notification or background contract.
