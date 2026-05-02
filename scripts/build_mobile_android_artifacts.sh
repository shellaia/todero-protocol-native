#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
read_cargo_version() {
  python3 -c 'import sys; from pathlib import Path; import tomllib; data = tomllib.loads(Path(sys.argv[1]).read_text(encoding="utf-8")); version = data.get("workspace", {}).get("package", {}).get("version") or data.get("package", {}).get("version"); print(version) if version else (_ for _ in ()).throw(SystemExit("missing version in Cargo.toml"))' "${REPO_ROOT}/Cargo.toml"
}
VERSION="${MOBILE_VERSION:-$(read_cargo_version)}"
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
  local symbols_dir="${OUT_ROOT}/symbols/${abi}"

  mkdir -p "${lib_dir}" "${symbols_dir}"
  require_target "${rust_target}"

  local cc="${TOOLCHAIN}/${clang_prefix}${MIN_SDK}-clang"
  local cxx="${TOOLCHAIN}/${clang_prefix}${MIN_SDK}-clang++"
  local ar="${TOOLCHAIN}/llvm-ar"
  local ranlib="${TOOLCHAIN}/llvm-ranlib"
  local strip_bin="${TOOLCHAIN}/llvm-strip"
  local objcopy_bin="${TOOLCHAIN}/llvm-objcopy"
  local target_upper
  target_upper="$(printf '%s' "${rust_target}" | tr '[:lower:]-' '[:upper:]_')"

  # Ensure native deps (openssl-sys) use the NDK toolchain rather than trying to
  # call non-existent unversioned wrappers like `aarch64-linux-android-clang`.
  export PATH="${TOOLCHAIN}:${PATH}"
  export CC="${cc}"
  export CXX="${cxx}"
  export AR="${ar}"
  export RANLIB="${ranlib}"

  # Target-specific overrides used by cc-rs.
  local rust_target_env
  rust_target_env="$(printf '%s' "${rust_target}" | tr '[:lower:]-' '[:lower:]_')"
  export "CC_${rust_target_env}=${cc}"
  export "CXX_${rust_target_env}=${cxx}"
  export "AR_${rust_target_env}=${ar}"

  # Rust linker for this target.
  export "CARGO_TARGET_${target_upper}_LINKER=${cc}"

  cargo build \
    --manifest-path "${WORKSPACE_MANIFEST}" \
    -p v3-ffi \
    --release \
    --target "${rust_target}"

  local built_so="${REPO_ROOT}/target/${rust_target}/release/libv3_ffi.so"
  local shipped_so="${lib_dir}/libv3_ffi.so"
  local debug_so="${symbols_dir}/libv3_ffi.so.debug"

  cp -f "${built_so}" "${shipped_so}"
  "${objcopy_bin}" --only-keep-debug "${built_so}" "${debug_so}"
  "${strip_bin}" --strip-debug --strip-unneeded "${shipped_so}"
  "${objcopy_bin}" --add-gnu-debuglink="${debug_so}" "${shipped_so}" || true
}

rm -rf "${OUT_ROOT}/jniLibs" "${OUT_ROOT}/symbols"
mkdir -p "${OUT_ROOT}/jniLibs" "${OUT_ROOT}/symbols"

build_abi "arm64-v8a" "aarch64-linux-android" "aarch64-linux-android"
build_abi "x86_64" "x86_64-linux-android" "x86_64-linux-android"

(
  cd "${OUT_ROOT}"
  zip -qr "todero-native-mobile-android-${VERSION}.zip" jniLibs
  zip -qr "todero-native-mobile-android-symbols-${VERSION}.zip" symbols
)

cat > "${OUT_ROOT}/metadata.json" <<EOF
{
  "name": "todero-native-mobile-android",
  "version": "${VERSION}",
  "commit": "$(git -C "${REPO_ROOT}" rev-parse HEAD)",
  "built_at": "$(date -u +%Y-%m-%dT%H:%M:%SZ)",
  "artifacts": {
    "jniLibs": "jniLibs",
    "archive": "todero-native-mobile-android-${VERSION}.zip",
    "symbols": "symbols",
    "symbols_archive": "todero-native-mobile-android-symbols-${VERSION}.zip"
  }
}
EOF
