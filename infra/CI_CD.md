# CI/CD

## Pull request pipeline

- cargo fmt --check
- cargo clippy -- -D warnings
- cargo test
- frontend typecheck/lint/test
- schema/OpenAPI checks
- SQLx offline metadata check
- dependency audit
- secret scan
- container build
- SBOM
- vulnerability scan

## Release

- signed artifacts;
- semantic version;
- changelog;
- database migration plan;
- rollback plan;
- desktop/mobile channel;
- Tauri updater signing;
- staging smoke test;
- broker integrations default disabled.

## Branch protection

- reviews vereist voor risk engine, execution en auth;
- CODEOWNERS;
- geen force push;
- signed commits/tags waar haalbaar.
