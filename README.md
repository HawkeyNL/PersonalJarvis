# PersonalJarvis

PersonalJarvis is proprietary personal AI software.

This repository is publicly visible for source-reference purposes only. It is
not open source: use, copying, modification, distribution and commercial use
are prohibited except with prior written permission from the copyright holder.
See [LICENSE](LICENSE).

The desktop client now lives in
[HawkeyNL/PersonalJarvisApp](https://github.com/HawkeyNL/PersonalJarvisApp).
This repository owns Core/Home Node, Core Admin, the CLI, and native Android/iOS.
Core releases (`vX.Y.Z`) and desktop releases (`app-vX.Y.Z`) are independent.
The authoritative shared client protocol remains in `crates/client-core`;
the desktop consumes an exact reviewed Git revision, not a sibling checkout.
The Home Node mirrors signed public desktop releases outbound and serves them
only to authenticated enrolled clients. See
[application update deployment](docs/app-updates/PRIVATE_RELEASES.md).

Home Node model-routing and credential operations are documented in
[docs/MODEL_ROUTING_OPERATIONS.md](docs/MODEL_ROUTING_OPERATIONS.md).

For a provisioned Home Node, the canonical root-operated owner interface is
[`sudo jarvis ...`](docs/JARVIS_ADMIN_CLI.md). It wraps the existing verified
release updater, model policy, credential manager, private-agent updater, and
bounded diagnostics without giving those privileges to Jarvis Core.
