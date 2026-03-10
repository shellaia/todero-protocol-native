#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
VERSION="${MOBILE_VERSION:-$(tr -d '[:space:]' < "${REPO_ROOT}/version.txt")}"
OUT_ROOT="${MOBILE_OUT_DIR:-${REPO_ROOT}/dist/mobile/${VERSION}/ios}"
WORKSPACE_MANIFEST="${REPO_ROOT}/Cargo.toml"
CRATE_NAME="v3-ffi"

HEADERS_DIR="${OUT_ROOT}/headers"
MODULES_DIR="${OUT_ROOT}/modules"
DEVICE_DIR="${OUT_ROOT}/iphoneos"
SIM_DIR="${OUT_ROOT}/iphonesimulator"

mkdir -p "${HEADERS_DIR}" "${MODULES_DIR}" "${DEVICE_DIR}" "${SIM_DIR}"

require_target() {
  local target="$1"
  if ! rustup target list --installed | grep -qx "${target}"; then
    echo "error: Missing Rust target ${target}. Run: rustup target add ${target}" >&2
    exit 1
  fi
}

build_target() {
  local rust_target="$1"
  cargo rustc \
    --manifest-path "${WORKSPACE_MANIFEST}" \
    -p "${CRATE_NAME}" \
    --release \
    --target "${rust_target}" \
    --lib \
    -- \
    --crate-type cdylib
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

cat > "${MODULES_DIR}/module.modulemap" <<'EOF'
framework module v3_ffi {
  umbrella header "v3_ffi.h"
  export *
  module * { export * }
}
EOF

require_target "aarch64-apple-ios"
require_target "aarch64-apple-ios-sim"

# Keep deployment target consistent across dependencies and the consuming app.
export IPHONEOS_DEPLOYMENT_TARGET="${IPHONEOS_DEPLOYMENT_TARGET:-13.0}"

build_target "aarch64-apple-ios"
build_target "aarch64-apple-ios-sim"

cp -f \
  "${REPO_ROOT}/target/aarch64-apple-ios/release/libv3_ffi.dylib" \
  "${DEVICE_DIR}/libv3_ffi.dylib"

cp -f \
  "${REPO_ROOT}/target/aarch64-apple-ios-sim/release/libv3_ffi.dylib" \
  "${SIM_DIR}/libv3_ffi.dylib"

cat > "${OUT_ROOT}/metadata.json" <<EOF
{
  "name": "todero-native-mobile-ios",
  "version": "${VERSION}",
  "commit": "$(git -C "${REPO_ROOT}" rev-parse HEAD)",
  "built_at": "$(date -u +%Y-%m-%dT%H:%M:%SZ)",
  "artifacts": {
    "device": "iphoneos/libv3_ffi.dylib",
    "simulator_arm64": "iphonesimulator/libv3_ffi.dylib"
  }
}
EOF
