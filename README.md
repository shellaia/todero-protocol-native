# todero-protocol-native

Standalone Rust workspace for Todero Protocol V3 native runtime and FFI.

## Ownership Contract

`todero-protocol-native` is the sole publisher for `todero-native` distribution assets:

- native channel artifacts under this repo's S3 prefix
- `Formula/todero-native.rb` in `biblip/homebrew-todero`

No other repository should publish `todero-native` artifacts or formula updates.

## Workspace

- `v3-ffi`: native bridge crate (`cdylib`) exported to JVM integrations.
- `v3-core`, `v3-wire`, `v3-store`, `v3-client-runtime`, `v3-server-runtime`, `v3-crypto`, `v3-nat`, `v3-transport-udp`, `v3-orchestrator-api`: protocol/runtime crates.
- `scripts/`: local verification and integration helper scripts.
- `tests/coturn/`: coturn interop scenarios.

## Build

```bash
cargo check
cargo test --workspace
cargo check -p v3-ffi --release
```

Build FFI shared library:

```bash
cargo build -p v3-ffi --release
```

Expected library output:

- Linux: `target/release/libv3_ffi.so`
- macOS: `target/release/libv3_ffi.dylib`

## Local Integration Notes

For local JVM usage, ensure `java.library.path` includes the directory containing `libv3_ffi`:

```bash
java -Djava.library.path=/path/to/target/release -jar your-app.jar
```

## Sync From Monorepo Source

This repo includes a bootstrap sync utility:

```bash
scripts/sync-from-todero.sh
```

Optional custom source path:

```bash
scripts/sync-from-todero.sh /absolute/path/to/todero/protocol-v3
```

## Release Metadata

- Version and tag policy: `docs/versioning-policy.md`
- Native artifact and manifest contract: `docs/artifact-contract.md`
- Rollback runbook (alias repoint): `docs/rollback-runbook.md`

## Release Automation (Patch)

Use the release helper to cut patch releases (`X.Y.Z -> X.Y.(Z+1)`):

```bash
scripts/release/publish-release --dry-run
scripts/release/publish-release
```

Rules:
- Must run from `main` (script verifies and fails otherwise).
- Uses `version.txt` as source of truth.
- Commits/pushes `version.txt` first, then creates/pushes `vX.Y.Z` tag.
- Major/minor bumps are manual changes to `version.txt` outside this script.

## Brew Install (Direct Formula URL)

Recommended:

```bash
brew tap biblip/homebrew-todero
brew install todero-native
```

After install, resolve native path with:

```bash
tninfo --libdir
export TODERO_V3_NATIVE_PATH="$(tninfo --libdir)"
```

## Linux Install (APT/YUM)

The release workflow now publishes real Linux packages:

- `todero-native_<version>_amd64.deb`
- `todero-native_<version>_arm64.deb`
- `todero-native-<version>-1.x86_64.rpm`
- `todero-native-<version>-1.aarch64.rpm`

All Linux packages install:

- native library under `/usr/lib/todero/native/<target-id>/`
- symlink `/usr/lib/todero/native/current`
- resolver command `/usr/bin/tninfo --libdir`

For canonical S3 snapshot/alias path layout, see `docs/artifact-contract.md` (single source of truth).

### APT setup/install

```bash
curl -fsSL https://apt.social100.com/<S3_PREFIX>/todero-native-repo-apt.gpg \
  | sudo gpg --dearmor -o /usr/share/keyrings/todero-native-archive-keyring.gpg

echo "deb [signed-by=/usr/share/keyrings/todero-native-archive-keyring.gpg] \
https://apt.social100.com/<S3_PREFIX>/channels/stable stable main" \
  | sudo tee /etc/apt/sources.list.d/todero-native.list >/dev/null

sudo apt update
sudo apt install -y todero-native
```

### YUM/DNF setup/install

```bash
sudo tee /etc/yum.repos.d/todero-native.repo >/dev/null <<'EOF'
[todero-native]
name=Todero Native
baseurl=https://yum.social100.com/<S3_PREFIX>/
enabled=1
gpgcheck=1
repo_gpgcheck=1
gpgkey=https://yum.social100.com/<S3_PREFIX>/todero-native-repo-yum.asc
EOF

sudo dnf clean all || true
sudo dnf makecache
sudo dnf install -y todero-native
```

## Release Workflow Secrets

`protocol-native-release.yml` requires:

- `AWS_ACCESS_KEY_ID`
- `AWS_SECRET_ACCESS_KEY`
- `AWS_REGION`
- `S3_BUCKET_APT`
- `S3_BUCKET_YUM`
- `S3_BUCKET_BREW`
- `S3_PREFIX`
- `APT_GPG_PRIVATE_KEY`
- `APT_GPG_PASSPHRASE` (optional if key has no passphrase)
- `APT_GPG_KEY_ID` (optional; auto-detected if omitted)
- `YUM_GPG_PRIVATE_KEY`
- `YUM_GPG_PASSPHRASE` (optional if key has no passphrase)
- `YUM_GPG_KEY_ID` (optional; auto-detected if omitted)
- `BREW_TAP_REPO` (example: `biblip/homebrew-todero`)
- `BREW_TAP_TOKEN` (token with write access to tap repo)

Required repository variables:

- `CLOUDFRONT_DISTRIBUTION_ID_APT`
- `CLOUDFRONT_DISTRIBUTION_ID_YUM`
- `CLOUDFRONT_DISTRIBUTION_ID_BREW`

## Release Lifecycle And Failure Handling

Canonical release lifecycle:
1. Cut tag via `scripts/release/publish-release` (or push valid `vX.Y.Z` tag).
2. Workflow builds/signs/verifies artifacts and repository metadata.
3. Workflow publishes snapshots first, then alias payloads.
4. Workflow updates per-bucket history manifest (`releases/manifest.json`).
5. Workflow invalidates CloudFront alias metadata paths.
6. Workflow publishes brew tap formula and validates install on arm mac.

Failure handling:
- Build/manifest failure:
  - fix code or metadata and re-run tagged release.
- Signing/verification failure:
  - verify signing secrets and key IDs; do not publish unsigned artifacts.
- Publish/manifest-pointer failure:
  - inspect `releases/manifest.json` in each bucket and re-run release.
- Stale alias behavior:
  - inspect CloudFront invalidation outputs and metadata TTL objects.
- Emergency rollback:
  - use `docs/rollback-runbook.md` and `scripts/rollback_alias_repoint.sh` (alias-only repoint, no snapshot deletion).
