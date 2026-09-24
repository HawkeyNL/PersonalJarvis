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
serves them only to authenticated enrolled clients. iOS is validated in client
CI and installed locally by the owner through Xcode; it has no server-delivered
artifact. See [application update deployment](docs/app-updates/CLIENT_RELEASES.md).

Home Node model-routing and credential operations are documented in
[docs/MODEL_ROUTING_OPERATIONS.md](docs/MODEL_ROUTING_OPERATIONS.md).

For a provisioned Home Node, the canonical root-operated owner interface is
[`sudo jarvis ...`](docs/JARVIS_ADMIN_CLI.md). It wraps the existing verified
release updater, model policy, credential manager, private-agent updater, and
bounded diagnostics without giving those privileges to Jarvis Core.

Client model controls use a fresh native OS authentication prompt and a
device-signed, short-lived approval bound to the exact model, requested state,
and policy revision. Core reports success only after protected disk readback
and live router activation agree. A credential or ordinary session alone does
not authorize a model change.

Releases declaring `tooling.model_policy_directory: 1` include the native
policy-layout migration helper and matching broker unit. Normal activation
migrates the policy; rollback to a legacy release exports the current owner
choices rather than restoring stale model grants. Repeated installation does
not reimport the legacy copy. An ambiguous/interrupted layout fails closed and
requires owner recovery instead of guessing which copy is authoritative.
