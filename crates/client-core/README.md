# Jarvis client core

`jarvis-client-core` is the small, platform-neutral compatibility boundary
between Jarvis clients and the v1 Home Node API. It owns wire DTOs, fixed input
limits, protocol-version constants, and the canonical bytes for device-pairing
approval. It deliberately does not perform HTTP requests, persist credentials,
prompt for biometrics, or own application UI state.

## UniFFI assessment

UniFFI is a good future fit for the pure functions and records in this crate:
Swift and Kotlin can consume one implementation of fixed-length validation and
canonical protocol encoding, while Rust callers use the crate directly. A thin
FFI facade should expose UUIDs as strings, timestamps as signed epoch seconds,
and byte arrays with explicit length checks; `uuid::Uuid` and
`time::OffsetDateTime` should remain internal to that facade.

Do not put Keychain/Keystore/Secret Service access, biometric policy, HTTP,
discovery, background execution, or UI orchestration behind UniFFI. Those are
platform lifecycle and permission concerns. Private key bytes should not be
passed through a generic FFI signing function: prefer narrowly named,
domain-bound operations and a platform-owned signer. Adding UniFFI now would
also commit generated Swift/Kotlin APIs before the native application shells
exist, so this crate currently remains an ordinary Rust library with golden
vectors that any later binding must preserve.
