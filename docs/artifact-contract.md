# Native Artifact Naming Contract

This document defines the required artifact naming and manifest contract consumed by downstream packaging workflows.

## Native Tarball Names

Per target, publish:

`todero-native-<target>-<version>.tar.gz`

Examples:

- `todero-native-linux-x86_64-gnu-0.1.80.tar.gz`
- `todero-native-linux-aarch64-gnu-0.1.80.tar.gz`
- `todero-native-darwin-aarch64-0.1.80.tar.gz`

## Mobile Artifact Names

Per version, publish mobile artifacts under a dedicated mobile distribution flow:

- Apple packaged output root:
  - `dist/mobile/<version>/apple/`
- Android packaged output root:
  - `dist/mobile/<version>/android/`

Required mobile outputs:

- Apple:
  - `apple/iphoneos/libv3_ffi.dylib`
  - `apple/iphoneos/v3_ffi.framework/`
  - `apple/iphonesimulator/libv3_ffi.dylib`
  - `apple/iphonesimulator/v3_ffi.framework/`
  - `apple/macos/libv3_ffi.dylib`
  - `apple/macos/v3_ffi.framework/`
  - `apple/v3_ffi.xcframework`
  - `apple/v3_ffi.xcframework.zip`
  - `apple/metadata.json`
  - published release asset:
    - `v3_ffi.xcframework.zip`
  - published release metadata asset:
    - `todero-native-mobile-apple-metadata-<version>.json`
- Android:
  - `android/jniLibs/arm64-v8a/libv3_ffi.so`
  - `android/jniLibs/x86_64/libv3_ffi.so`
  - `android/symbols/arm64-v8a/libv3_ffi.so.debug`
  - `android/symbols/x86_64/libv3_ffi.so.debug`
  - `android/todero-native-mobile-android-<version>.zip`
  - `android/todero-native-mobile-android-symbols-<version>.zip`
  - `android/metadata.json`
  - published release metadata asset:
    - `todero-native-mobile-android-metadata-<version>.json`
  - published release symbols asset:
    - `todero-native-mobile-android-symbols-<version>.zip`
  - published Maven Central artifact:
    - `com.shellaia.todero:todero-v3-ffi-android:<version>`

Android symbol policy:

- shipped `jniLibs/*.so` artifacts are stripped for release size
- debug symbols are preserved separately under `android/symbols/`
- the Maven Central AAR packages stripped runtime libraries only

Per tarball checksum:

`todero-native-<target>-<version>.tar.gz.sha256`

Linux package outputs:

- Debian:
  - `todero-native_<version>_amd64.deb`
  - `todero-native_<version>_arm64.deb`
- RPM:
  - `todero-native-<version>-1.x86_64.rpm`
  - `todero-native-<version>-1.aarch64.rpm`

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
  - `toolchain`
  - `sha256`
  - `commit`
  - `built_at`
  - `library`

## Linux Package Content Contract

Each Linux package (`.deb`/`.rpm`) installs:

- `/usr/lib/todero/native/<target-id>/libv3_ffi.so`
- `/usr/lib/todero/native/<target-id>/metadata.json`
- `/usr/lib/todero/native/current` (symlink)
- `/usr/bin/tninfo --libdir` (prints `/usr/lib/todero/native/current`)
- `/usr/lib/todero-native/profile-env-setup.sh` (startup profile helper)

Installer lifecycle contract:
- post-install applies startup snippets for:
  - `~/.bashrc`
  - `~/.zshrc`
  - `~/.config/fish/conf.d/todero-native.fish`
- snippets must be marker-managed and guarded:
  - `# >>> todero-native >>>`
  - `# <<< todero-native <<<`
  - bash/zsh guard: `command -v tninfo >/dev/null 2>&1`
  - fish guard: `type -q tninfo`
- uninstall does not mutate profiles; it only prints manual cleanup guidance.

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

- `<base>/todero-native-darwin-aarch64-<version>.tar.gz`

Formula install contract:
- installs `tninfo` and `libexec/profile-env-setup.sh`
- runs profile setup during `post_install` to apply guarded startup snippets.

## Backing Store Publication Paths

Given `S3_PREFIX=<prefix>`:

- Channel alias roots:
  - APT: `s3://<apt-bucket>/<prefix>/channels/stable/`
  - YUM: `s3://<yum-bucket>/<prefix>/channels/stable/`
  - BREW: `s3://<brew-bucket>/<prefix>/channels/stable/`
- Immutable snapshot roots:
  - APT: `s3://<apt-bucket>/<prefix>/releases/<version>/`
  - YUM: `s3://<yum-bucket>/<prefix>/releases/<version>/`
  - BREW: `s3://<brew-bucket>/<prefix>/releases/<version>/`
- Native manifest history index:
  - `s3://<bucket>/<prefix>/releases/manifest.json`

Downstream consumers are expected to fetch artifacts from channel alias path:

- YUM/BREW: `<prefix>/channels/stable/todero-native-<target>-<version>.tar.gz`
- YUM/BREW: `<prefix>/channels/stable/todero-release-manifest-<version>.json`
- APT: `<prefix>/channels/stable/{dists,pool,...}`

APT repository layout under apt alias root:

- `pool/main/t/todero-native/*.deb`
- `dists/stable/main/binary-amd64/Packages{,.gz}`
- `dists/stable/main/binary-arm64/Packages{,.gz}`
- `dists/stable/Release`
- `dists/stable/InRelease`
- `dists/stable/Release.gpg`

YUM repository layout under alias root:

- `packages/*.rpm`
- `repodata/*`
- `repodata/repomd.xml.asc`
