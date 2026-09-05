# Jarvis client monorepo boundary

All editable end-user client source is maintained in
`HawkeyNL/PersonalJarvisApp`:

```text
desktop/
android/
ios/
```

This Core repository intentionally contains no canonical desktop, Android, or
iOS source and no client signing workflow. It retains the server/API, Core
Admin, Home Node deployment, authenticated update mirror/delivery, and the sole
authoritative Rust `crates/client-core` implementation.

The client repository was migrated as a snapshot without rewriting this
repository's public history. The desktop depends on `jarvis-client-core` through
an exact Git revision. Android and iOS currently keep their reviewed native DTOs;
future schema generation should originate from an explicit authoritative Core
contract rather than copying private server implementation details.

One application SemVer coordinates all three clients. Android versionCode and
iOS build number advance independently. Core releases remain independently
versioned. See [client update delivery](app-updates/CLIENT_RELEASES.md).
