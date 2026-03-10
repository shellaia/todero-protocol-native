#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
VERSION="${MOBILE_VERSION:-$(tr -d '[:space:]' < "${REPO_ROOT}/version.txt")}"
OUT_ROOT="${MOBILE_OUT_DIR:-${REPO_ROOT}/dist/mobile/${VERSION}/android}"
SDK_ROOT="${ANDROID_SDK_ROOT:?missing ANDROID_SDK_ROOT}"
NDK_VERSION="${ANDROID_NDK_VERSION:?missing ANDROID_NDK_VERSION}"
MIN_SDK="${ANDROID_MIN_SDK:-21}"
WORKSPACE_MANIFEST="${REPO_ROOT}/Cargo.toml"

NDK_ROOT="${SDK_ROOT}/ndk/${NDK_VERSION}"
if [[ ! -d "${NDK_ROOT}" ]]; then
  echo "error: Android NDK ${NDK_VERSION} not found under ${SDK_ROOT}/ndk" >&2
  exit 1
fi

case "$(uname -s)-$(uname -m)" in
  Linux-x86_64) HOST_TAG="linux-x86_64" ;;
  Darwin-arm64) HOST_TAG="darwin-arm64" ;;
  Darwin-x86_64) HOST_TAG="darwin-x86_64" ;;
  *)
    echo "error: Unsupported host $(uname -s)-$(uname -m) for Android NDK toolchain" >&2
    exit 1
    ;;
esac

TOOLCHAIN="${NDK_ROOT}/toolchains/llvm/prebuilt/${HOST_TAG}/bin"
if [[ ! -d "${TOOLCHAIN}" ]]; then
  echo "error: Android NDK toolchain not found at ${TOOLCHAIN}" >&2
  exit 1
fi

require_target() {
  local target="$1"
  if ! rustup target list --installed | grep -qx "${target}"; then
    echo "error: Missing Rust target ${target}. Run: rustup target add ${target}" >&2
    exit 1
  fi
}

build_abi() {
  local abi="$1"
  local rust_target="$2"
  local clang_prefix="$3"
  local lib_dir="${OUT_ROOT}/jniLibs/${abi}"

  mkdir -p "${lib_dir}"
  require_target "${rust_target}"

  local linker="${TOOLCHAIN}/${clang_prefix}${MIN_SDK}-clang"
  local ar="${TOOLCHAIN}/llvm-ar"
  local target_upper
  target_upper="$(printf '%s' "${rust_target}" | tr '[:lower:]-' '[:upper:]_')"
  export "CARGO_TARGET_${target_upper}_LINKER=${linker}"
  export AR="${ar}"

  cargo build \
    --manifest-path "${WORKSPACE_MANIFEST}" \
    -p v3-ffi \
    --release \
    --target "${rust_target}"

  cp -f \
    "${REPO_ROOT}/target/${rust_target}/release/libv3_ffi.so" \
    "${lib_dir}/libv3_ffi.so"
}

rm -rf "${OUT_ROOT}/jniLibs"
mkdir -p "${OUT_ROOT}/jniLibs"

build_abi "arm64-v8a" "aarch64-linux-android" "aarch64-linux-android"
build_abi "x86_64" "x86_64-linux-android" "x86_64-linux-android"

(
  cd "${OUT_ROOT}"
  zip -qr "todero-native-mobile-android-${VERSION}.zip" jniLibs
)

cat > "${OUT_ROOT}/metadata.json" <<EOF
{
  "name": "todero-native-mobile-android",
  "version": "${VERSION}",
  "commit": "$(git -C "${REPO_ROOT}" rev-parse HEAD)",
  "built_at": "$(date -u +%Y-%m-%dT%H:%M:%SZ)",
  "artifacts": {
    "jniLibs": "jniLibs",
    "archive": "todero-native-mobile-android-${VERSION}.zip"
  }
}
EOF
