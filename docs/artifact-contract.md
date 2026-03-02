# Native Artifact Naming Contract

This document defines the required artifact naming and manifest contract consumed by downstream packaging workflows.

## Native Tarball Names

Per target, publish:

`todero-native-<target>-<version>.tar.gz`

Examples:

- `todero-native-linux-x86_64-gnu-0.1.80.tar.gz`
- `todero-native-linux-aarch64-gnu-0.1.80.tar.gz`
- `todero-native-darwin-aarch64-0.1.80.tar.gz`

Per tarball checksum:

`todero-native-<target>-<version>.tar.gz.sha256`

Per signed artifact, detached signature:

- `APT` key signature: `<artifact>.apt.asc`
- `YUM` key signature: `<artifact>.yum.asc`

Examples:

- `todero-native-linux-x86_64-gnu-0.1.80.tar.gz.apt.asc`
- `todero-native-linux-x86_64-gnu-0.1.80.tar.gz.yum.asc`
- `todero-release-manifest-0.1.80.json.apt.asc`
- `todero-native.rb.apt.asc`

## Tarball Content Contract

Each tarball root folder is `<target>/` and must contain:

- shared library:
  - Linux: `libv3_ffi.so`
  - macOS: `libv3_ffi.dylib`
- `metadata.json` with at least:
  - `name`
  - `version`
  - `target_id`
  - `target_triple`
  - `os`
  - `arch`
  - `library`

## Release Manifest Name

Per version, publish:

`todero-release-manifest-<version>.json`

Example:

- `todero-release-manifest-0.1.80.json`

## Release Manifest Shape

Top-level fields:

- `version` (string)
- `release_tag` (string; `vX.Y.Z` when tag-triggered)
- `created_at_utc` (ISO-8601 UTC string)
- `native_artifacts` (array)
- `build` (object with workflow/run metadata)

## Signing Contract

The release workflow signs all published alias artifacts:

- `*.tar.gz`
- `*.sha256`
- `*.json`

It also publishes exported public keys:

- `todero-native-repo-apt.gpg` / `todero-native-repo-apt.asc`
- `todero-native-repo-yum.gpg` / `todero-native-repo-yum.asc`

Each `native_artifacts[]` item:

- `name`: `"todero-native"`
- `channel`: `"apt"` | `"yum"` | `"brew"`
- `target_id`: target identifier
- `artifact_path`: tarball file name
- `publication_path`: path under channel alias root
- `sha256`: artifact SHA-256 hex digest

## Brew Formula Contract

Published for brew channel:

- `todero-native.rb`

The formula URL points to the darwin arm64 archive under alias root:

- `<base>/native/todero-native-darwin-aarch64-<version>.tar.gz`

## Backing Store Publication Paths

Given `S3_PREFIX_NATIVE=<prefix>`:

- Alias root:
  - `s3://<bucket>/<prefix>/native/`
- Immutable snapshot root:
  - `s3://<bucket>/<prefix>/native/releases/<version>/`
- Native manifest history index:
  - `s3://<bucket>/<prefix>/native/releases/manifest.json`

Downstream consumers are expected to fetch artifacts from alias root path:

- `<prefix>/native/todero-native-<target>-<version>.tar.gz`
- `<prefix>/native/todero-release-manifest-<version>.json`
