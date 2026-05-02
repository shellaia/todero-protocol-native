# Versioning And Tag Policy

## Version Source Of Truth

- Workspace version is defined in `Cargo.toml` under `[workspace.package].version`.
- Release version format is semantic versioning: `MAJOR.MINOR.PATCH`.

## Git Tag Policy

- Release tags must be: `vX.Y.Z`.
- Examples:
  - `v0.1.0`
  - `v1.0.0`
- Tags without leading `v` are invalid for release automation.

## Release Alignment

- The release tag version (`vX.Y.Z`) must match workspace version (`X.Y.Z`) for a release cut.
- CI/release automation should fail if tag and workspace version diverge.

## Automated Patch Release Cut

- Use `scripts/release/publish-release` to cut patch releases.
- Script enforces `main` branch and clean worktree.
- Script updates `Cargo.toml`, commits/pushes it, then creates/pushes `vX.Y.Z`.
- Script only increments patch (`z`) and never changes major/minor.

## Pre-release Builds

- Pre-release builds for validation can use non-tag runs.
- Published release artifacts are only produced from valid `vX.Y.Z` tags.
