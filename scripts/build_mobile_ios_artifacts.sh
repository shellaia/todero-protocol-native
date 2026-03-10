#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
VERSION="${MOBILE_VERSION:-$(tr -d '[:space:]' < "${REPO_ROOT}/version.txt")}"
OUT_ROOT="${MOBILE_OUT_DIR:-${REPO_ROOT}/dist/mobile/${VERSION}/ios}"
WORKSPACE_MANIFEST="${REPO_ROOT}/Cargo.toml"
CRATE_NAME="v3-ffi"

HEADERS_DIR="${OUT_ROOT}/headers"
DEVICE_DIR="${OUT_ROOT}/iphoneos"
SIM_DIR="${OUT_ROOT}/iphonesimulator"

mkdir -p "${HEADERS_DIR}" "${DEVICE_DIR}" "${SIM_DIR}"

require_target() {
  local target="$1"
  if ! rustup target list --installed | grep -qx "${target}"; then
    echo "error: Missing Rust target ${target}. Run: rustup target add ${target}" >&2
    exit 1
  fi
}

build_target() {
  local rust_target="$1"
  # v3-ffi defines multiple crate-types. For iOS packaging we only need the
  # staticlib output (libv3_ffi.a). Building cdylib can fail on iOS toolchains.
  cargo rustc \
    --manifest-path "${WORKSPACE_MANIFEST}" \
    -p "${CRATE_NAME}" \
    --release \
    --target "${rust_target}" \
    --lib \
    -- \
    --crate-type staticlib
}

cat > "${HEADERS_DIR}/v3_ffi.h" <<'EOF'
#ifndef TODERO_V3_FFI_H
#define TODERO_V3_FFI_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

uint32_t v3_ffi_abi_version(void);
uint64_t v3_ffi_new_client(
    uint32_t cid,
    uint64_t msr_interval_ms,
    uint64_t miss_window_count,
    uint64_t disconnect_window_ms);
void v3_ffi_free(uint64_t handle_id);

#ifdef __cplusplus
}
#endif

#endif
EOF

require_target "aarch64-apple-ios"
require_target "aarch64-apple-ios-sim"

# Keep deployment target consistent across dependencies (notably openssl-sys).
export IPHONEOS_DEPLOYMENT_TARGET="${IPHONEOS_DEPLOYMENT_TARGET:-12.0}"

build_target "aarch64-apple-ios"
build_target "aarch64-apple-ios-sim"

cp -f \
  "${REPO_ROOT}/target/aarch64-apple-ios/release/libv3_ffi.a" \
  "${DEVICE_DIR}/libv3_ffi.a"

cp -f \
  "${REPO_ROOT}/target/aarch64-apple-ios-sim/release/libv3_ffi.a" \
  "${SIM_DIR}/libv3_ffi.a"

cat > "${OUT_ROOT}/metadata.json" <<EOF
{
  "name": "todero-native-mobile-ios",
  "version": "${VERSION}",
  "commit": "$(git -C "${REPO_ROOT}" rev-parse HEAD)",
  "built_at": "$(date -u +%Y-%m-%dT%H:%M:%SZ)",
  "artifacts": {
    "device": "iphoneos/libv3_ffi.a",
    "simulator_arm64": "iphonesimulator/libv3_ffi.a"
  }
}
EOF
