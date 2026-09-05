# PersonalJarvis

PersonalJarvis is proprietary personal AI software.

This repository is publicly visible for source-reference purposes only. It is
not open source: use, copying, modification, distribution and commercial use
are prohibited except with prior written permission from the copyright holder.
See [LICENSE](LICENSE).

All end-user clients now live in
[HawkeyNL/PersonalJarvisApp](https://github.com/HawkeyNL/PersonalJarvisApp).
That client monorepo owns desktop, Android, and iOS. This repository owns
Core/Home Node, Core Admin, the CLI, server-side update mirroring, and the
authoritative shared protocol. Core releases (`vX.Y.Z`) and application
releases (`app-vX.Y.Z`) are independent.
The authoritative shared client protocol remains in `crates/client-core`;
the desktop consumes an exact reviewed Git revision, not a sibling checkout.
The Home Node mirrors signed public desktop/Android artifacts outbound and
serves them only to authenticated enrolled clients; iOS uses TestFlight/App
Store distribution. See [application update deployment](docs/app-updates/CLIENT_RELEASES.md).

Home Node model-routing and credential operations are documented in
[docs/MODEL_ROUTING_OPERATIONS.md](docs/MODEL_ROUTING_OPERATIONS.md).

For a provisioned Home Node, the canonical root-operated owner interface is
[`sudo jarvis ...`](docs/JARVIS_ADMIN_CLI.md). It wraps the existing verified
release updater, model policy, credential manager, private-agent updater, and
bounded diagnostics without giving those privileges to Jarvis Core.
