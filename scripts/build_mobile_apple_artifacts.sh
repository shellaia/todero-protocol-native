#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
VERSION="${MOBILE_VERSION:-$(python3 - <<'PY' "${REPO_ROOT}/Cargo.toml"
import re
import sys
from pathlib import Path
text = Path(sys.argv[1]).read_text(encoding='utf-8')
match = re.search(r'(?ms)^\[workspace\.package\].*?^version\s*=\s*"([^"]+)"', text)
if not match:
    match = re.search(r'(?m)^version\s*=\s*"([^"]+)"', text)
if not match:
    raise SystemExit('missing version in Cargo.toml')
print(match.group(1))
PY
)}"
OUT_ROOT="${MOBILE_OUT_DIR:-${REPO_ROOT}/dist/mobile/${VERSION}/apple}"
WORKSPACE_MANIFEST="${REPO_ROOT}/Cargo.toml"
CRATE_NAME="v3-ffi"

HEADERS_DIR="${OUT_ROOT}/headers"
MODULES_DIR="${OUT_ROOT}/modules"
IOS_DEVICE_DIR="${OUT_ROOT}/iphoneos"
IOS_SIM_DIR="${OUT_ROOT}/iphonesimulator"
MACOS_DIR="${OUT_ROOT}/macos"

mkdir -p "${HEADERS_DIR}" "${MODULES_DIR}" "${IOS_DEVICE_DIR}" "${IOS_SIM_DIR}" "${MACOS_DIR}"

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
require_target "x86_64-apple-ios"
require_target "aarch64-apple-darwin"

export IPHONEOS_DEPLOYMENT_TARGET="${IPHONEOS_DEPLOYMENT_TARGET:-13.0}"
export MACOSX_DEPLOYMENT_TARGET="${MACOSX_DEPLOYMENT_TARGET:-14.0}"

build_target "aarch64-apple-ios"
build_target "aarch64-apple-ios-sim"
build_target "x86_64-apple-ios"
build_target "aarch64-apple-darwin"

cp -f "${REPO_ROOT}/target/aarch64-apple-ios/release/libv3_ffi.dylib" "${IOS_DEVICE_DIR}/libv3_ffi.dylib"
lipo -create \
  "${REPO_ROOT}/target/aarch64-apple-ios-sim/release/libv3_ffi.dylib" \
  "${REPO_ROOT}/target/x86_64-apple-ios/release/libv3_ffi.dylib" \
  -output "${IOS_SIM_DIR}/libv3_ffi.dylib"
cp -f "${REPO_ROOT}/target/aarch64-apple-darwin/release/libv3_ffi.dylib" "${MACOS_DIR}/libv3_ffi.dylib"

create_framework() {
  local source_lib="$1"
  local framework_root="$2"
  local bundle_id="$3"
  local min_os_key="$4"
  local min_os_value="$5"
  mkdir -p "${framework_root}/Headers" "${framework_root}/Modules"
  cp "${OUT_ROOT}/headers/v3_ffi.h" "${framework_root}/Headers/v3_ffi.h"
  cp "${OUT_ROOT}/modules/module.modulemap" "${framework_root}/Modules/module.modulemap"
  cp "${source_lib}" "${framework_root}/v3_ffi"
  chmod +x "${framework_root}/v3_ffi"
  install_name_tool -id "@rpath/v3_ffi.framework/v3_ffi" "${framework_root}/v3_ffi"
  {
    printf '%s\n' '<?xml version="1.0" encoding="UTF-8"?>'
    printf '%s\n' '<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">'
    printf '%s\n' '<plist version="1.0">'
    printf '%s\n' '<dict>'
    printf '%s\n' '  <key>CFBundleExecutable</key>'
    printf '%s\n' '  <string>v3_ffi</string>'
    printf '%s\n' '  <key>CFBundleIdentifier</key>'
    printf '%s\n' "  <string>${bundle_id}</string>"
    printf '%s\n' '  <key>CFBundleName</key>'
    printf '%s\n' '  <string>v3_ffi</string>'
    printf '%s\n' '  <key>CFBundlePackageType</key>'
    printf '%s\n' '  <string>FMWK</string>'
    printf '%s\n' '  <key>CFBundleShortVersionString</key>'
    printf '%s\n' "  <string>${VERSION}</string>"
    printf '%s\n' '  <key>CFBundleVersion</key>'
    printf '%s\n' "  <string>${VERSION}</string>"
    printf '%s\n' "  <key>${min_os_key}</key>"
    printf '%s\n' "  <string>${min_os_value}</string>"
    printf '%s\n' '</dict>'
    printf '%s\n' '</plist>'
  } > "${framework_root}/Info.plist"
}

create_framework "${IOS_DEVICE_DIR}/libv3_ffi.dylib" "${IOS_DEVICE_DIR}/v3_ffi.framework" "com.shellaia.todero.v3ffi.ios" "MinimumOSVersion" "13.0"
create_framework "${IOS_SIM_DIR}/libv3_ffi.dylib" "${IOS_SIM_DIR}/v3_ffi.framework" "com.shellaia.todero.v3ffi.iossim" "MinimumOSVersion" "13.0"
create_framework "${MACOS_DIR}/libv3_ffi.dylib" "${MACOS_DIR}/v3_ffi.framework" "com.shellaia.todero.v3ffi.macos" "LSMinimumSystemVersion" "14.0"

xcodebuild -create-xcframework \
  -framework "${IOS_DEVICE_DIR}/v3_ffi.framework" \
  -framework "${IOS_SIM_DIR}/v3_ffi.framework" \
  -framework "${MACOS_DIR}/v3_ffi.framework" \
  -output "${OUT_ROOT}/v3_ffi.xcframework" >/dev/null

rm -f "${OUT_ROOT}/v3_ffi.xcframework.zip"
ditto -c -k --sequesterRsrc --keepParent \
  "${OUT_ROOT}/v3_ffi.xcframework" \
  "${OUT_ROOT}/v3_ffi.xcframework.zip"

cat > "${OUT_ROOT}/metadata.json" <<EOF
{
  "name": "todero-native-mobile-apple",
  "version": "${VERSION}",
  "commit": "$(git -C "${REPO_ROOT}" rev-parse HEAD)",
  "built_at": "$(date -u +%Y-%m-%dT%H:%M:%SZ)",
  "artifacts": {
    "ios_device": "iphoneos/libv3_ffi.dylib",
    "ios_simulator_universal": "iphonesimulator/libv3_ffi.dylib",
    "macos_arm64": "macos/libv3_ffi.dylib",
    "xcframework": "v3_ffi.xcframework",
    "xcframework_zip": "v3_ffi.xcframework.zip"
  }
}
EOF
