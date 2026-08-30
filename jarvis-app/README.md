# Jarvis desktop app

`jarvis-app/` is the desktop Jarvis client. It uses one Vue 3 frontend and one
Tauri 2 Rust core for Linux, macOS, and Windows. The production mobile clients
live in `jarvis-ios/` and `jarvis-android/`; the generated Apple files under
`src-tauri/gen/apple/` are legacy reference material, not the mobile product.

The Home Node remains authoritative for identity enrollment, policy, chat, and
device revocation. The desktop app owns only its device key, session, local
preferences, and presentation.

Private signed desktop releases, the authenticated Home Node mirror and updater
operations are documented in
[`docs/app-updates/PRIVATE_RELEASES.md`](../docs/app-updates/PRIVATE_RELEASES.md).

## Common commands

Run these commands from this directory:

```bash
npm ci
npm run check
npm run desktop:check:native
npm run desktop:test:native
npm run desktop:dev
npm run desktop:build
```

`npm run check` is deterministic and does not download optional wake-word
models. Install those explicitly with `npm run setup-wakeword` before testing
the foreground detector. The downloaded models are ignored by Git.

## Ubuntu

Install Node.js 24, Rust, and the Tauri 2 Linux prerequisites:

```bash
sudo apt update
sudo apt install build-essential curl file libayatana-appindicator3-dev \
  libdbus-1-dev librsvg2-dev libssl-dev libwebkit2gtk-4.1-dev \
  pkg-config wget
```

The `libdbus-1-dev` package is also required by the persistent Secret Service
credential backend. A desktop Secret Service provider (for example GNOME
Keyring) must be running and unlocked. Jarvis fails clearly when persistent
secure storage is unavailable; it does not silently create a new plaintext
device key.

Validate the actual desktop target, not only shared Rust:

```bash
npm ci
npm run check
npm run desktop:test:native
npm run desktop:build
npm run desktop:dev
```

For an acceptance pass, launch the built artifact and verify Home Node health,
login, conversation load/send, reconnect after network interruption,
suspend/resume, logout/login, device reset/re-enrollment, microphone denial,
and wake-word foreground behavior when the optional models are installed.
Linux biometric hardware is currently reported as unsupported; the app-lock
uses trusted-device approval instead of crashing or accepting a password flag.

## macOS

Install Xcode Command Line Tools, Node.js 24, and Rust, then run the common
commands above. Validate both `npm run desktop:dev` and the bundled `.app` from
`npm run desktop:build`. macOS credentials use Keychain. The checked-in
`Info.plist` supplies the microphone usage description for unbundled debug
builds; release bundles receive their plist through Tauri.

Regression checklist: device-bound login, Keychain migration, Touch ID
cancellation, trusted-device unlock fallback, chat load/send, microphone
denial, foreground wake word, logout, and reset/re-enrollment.

## Windows

Install Node.js 24, Rust MSVC, Visual Studio C++ Build Tools, and WebView2.
Run the common commands in a Developer PowerShell or terminal. Credentials use
Windows Credential Manager. Windows packaging must be validated on Windows;
cross-compiling the Rust library is not an acceptance test.

## Credential classification and migration

- Secret: Ed25519 private device key and bearer session token. These are stored
  in Keychain, persistent Secret Service/keyutils storage, or Windows Credential
  Manager according to the operating system.
- Sensitive metadata: device identifier and the configured Home Node endpoint.
  These are not authentication secrets, but should not be logged unnecessarily.
- Ordinary metadata: UI preferences and the last selected conversation.

Older desktop installs may have `private_key` and `token` fields in
`auth.json`. On first access Jarvis writes each value to the OS credential store
and rewrites the file with only the device identifier. If that secure write
fails, migration fails closed and leaves the legacy data untouched for manual
recovery. A new plaintext fallback is never created.

The private Ed25519 seed is generated and used only by Rust. Tauri commands
return a public key or a bounded signature, never private key bytes. Login and
all authenticated API calls are completed by Rust using the credential-store
session; Vue receives only authentication status, device id and key presence,
never the bearer token. Changing the configured Home Node origin clears the old
session/device binding before the new origin is persisted.

## Current capability matrix

| Capability | Ubuntu | macOS | Windows |
| --- | --- | --- | --- |
| Desktop window features | Yes | Yes | Yes |
| OS-backed secure persistence | Secret Service/keyutils | Keychain | Credential Manager |
| Local biometric prompt | Unsupported in the current client | Touch ID | Windows Hello backend |
| Explicit microphone use | WebKit permission | WebKit permission | WebView2 permission |
| Foreground wake detector | Optional, requires models | Optional, requires models | Optional, requires models |
| Background wake/audio | No | No | No |
| Notifications | Not implemented | Not implemented | Not implemented |

Wake word is convenience only and never substitutes for a device-signed,
action-bound approval. Background listening, push notifications, APNs, and FCM
belong to separate native/mobile milestones.

## Troubleshooting

- `webkit2gtk-4.1.pc` missing: install `libwebkit2gtk-4.1-dev` and `pkg-config`.
- `dbus-1.pc` missing: install `libdbus-1-dev`.
- Secure credential storage unavailable on Ubuntu: start/unlock the desktop
  Secret Service provider, then retry. Jarvis intentionally does not fall back
  to `auth.json`.
- Wake detector reports model assets missing: run `npm run setup-wakeword`.
- Home Node is unreachable: verify the per-device Home Node origin in Settings,
  DNS/IP, and TLS. Release builds accept only credential-free HTTPS origins;
  loopback HTTP is available solely in debug builds for explicit local testing.
  The origin is ordinary local metadata and is reused after restart and device
  re-enrollment; no Home Node address is embedded during the build.
- Updates show incompatible: the active release requires a newer updater
  protocol than this client understands. Keep using the installed app and use
  an owner-reviewed compatible upgrade path.
