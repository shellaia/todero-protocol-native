# todero-protocol-native

Standalone Rust workspace for Todero Protocol V3 native runtime and FFI.

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

## Brew Install (Direct Formula URL)

After a release is published:

```bash
brew install --formula "https://<brew-domain>/<S3_PREFIX>/todero-native.rb"
```

After install, resolve native path with:

```bash
tninfo
export TODERO_V3_NATIVE_PATH="$(tninfo)"
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
- resolver command `/usr/bin/tninfo`

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
