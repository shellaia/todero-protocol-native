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
