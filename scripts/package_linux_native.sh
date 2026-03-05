#!/usr/bin/env bash
set -euo pipefail

VERSION="${1:?usage: package_linux_native.sh <version> <target_id> <lib_path> <metadata_path|-> [out_dir]}"
TARGET_ID="${2:?usage: package_linux_native.sh <version> <target_id> <lib_path> <metadata_path|-> [out_dir]}"
LIB_PATH="${3:?usage: package_linux_native.sh <version> <target_id> <lib_path> <metadata_path|-> [out_dir]}"
METADATA_PATH="${4:?usage: package_linux_native.sh <version> <target_id> <lib_path> <metadata_path|-> [out_dir]}"
OUT_DIR="${5:-dist/native/packages}"

if [[ ! -f "${LIB_PATH}" ]]; then
  echo "native library not found: ${LIB_PATH}" >&2
  exit 1
fi

case "${TARGET_ID}" in
  linux-x86_64-gnu)
    DEB_ARCH="amd64"
    RPM_ARCH="x86_64"
    LIB_NAME="libv3_ffi.so"
    ;;
  linux-aarch64-gnu)
    DEB_ARCH="arm64"
    RPM_ARCH="aarch64"
    LIB_NAME="libv3_ffi.so"
    ;;
  *)
    echo "unsupported linux target_id for packaging: ${TARGET_ID}" >&2
    exit 1
    ;;
esac

if ! command -v dpkg-deb >/dev/null 2>&1; then
  echo "dpkg-deb is required" >&2
  exit 1
fi
if ! command -v rpmbuild >/dev/null 2>&1; then
  echo "rpmbuild is required" >&2
  exit 1
fi

mkdir -p "${OUT_DIR}/deb" "${OUT_DIR}/rpm"

WORK_DIR="$(mktemp -d)"
trap 'rm -rf "${WORK_DIR}"' EXIT

ROOT="${WORK_DIR}/root"
NATIVE_DIR="${ROOT}/usr/lib/todero/native/${TARGET_ID}"
mkdir -p "${NATIVE_DIR}" "${ROOT}/usr/lib/todero/native" "${ROOT}/usr/bin" "${ROOT}/usr/share/doc/todero-native"

install -m 0755 "${LIB_PATH}" "${NATIVE_DIR}/${LIB_NAME}"
if [[ "${METADATA_PATH}" != "-" && -f "${METADATA_PATH}" ]]; then
  install -m 0644 "${METADATA_PATH}" "${NATIVE_DIR}/metadata.json"
fi
ln -sfn "${TARGET_ID}" "${ROOT}/usr/lib/todero/native/current"

cat > "${ROOT}/usr/bin/tninfo" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
if [[ "${1:-}" != "--libdir" ]]; then
  echo "usage: tninfo --libdir" >&2
  exit 2
fi
echo "/usr/lib/todero/native/current"
EOF
chmod 0755 "${ROOT}/usr/bin/tninfo"

cat > "${ROOT}/usr/share/doc/todero-native/README.md" <<'EOF'
Todero Protocol Native Runtime

Path resolver:
  tninfo --libdir

Environment:
  export TODERO_V3_NATIVE_PATH="$(tninfo --libdir)"
EOF

# Build .deb package
DEB_ROOT="${WORK_DIR}/deb-root"
cp -a "${ROOT}/." "${DEB_ROOT}/"
mkdir -p "${DEB_ROOT}/DEBIAN"
cat > "${DEB_ROOT}/DEBIAN/control" <<EOF
Package: todero-native
Version: ${VERSION}
Section: libs
Priority: optional
Architecture: ${DEB_ARCH}
Maintainer: Todero Team <security@shellaia.com>
Description: Todero Protocol V3 native runtime library
EOF
dpkg-deb --build --root-owner-group "${DEB_ROOT}" "${OUT_DIR}/deb/todero-native_${VERSION}_${DEB_ARCH}.deb"

# Build .rpm package
RPM_TOP="${WORK_DIR}/rpmbuild"
mkdir -p "${RPM_TOP}/"{BUILD,RPMS,SOURCES,SPECS,SRPMS}
tar -C "${ROOT}" -czf "${RPM_TOP}/SOURCES/root.tar.gz" .
RPM_VERSION="${VERSION//-/_}"
cat > "${RPM_TOP}/SPECS/todero-native.spec" <<EOF
Name:           todero-native
Version:        ${RPM_VERSION}
Release:        1
Summary:        Todero Protocol V3 native runtime library
License:        Proprietary
BuildArch:      ${RPM_ARCH}
Source0:        root.tar.gz

%description
Todero Protocol V3 native runtime library.

%prep
%setup -q -c -T
tar -xzf %{SOURCE0}

%build

%install
mkdir -p %{buildroot}
cp -a . %{buildroot}/

%files
/usr/bin/tninfo
/usr/lib/todero/native
/usr/share/doc/todero-native/README.md
EOF

rpmbuild --define "_topdir ${RPM_TOP}" -bb "${RPM_TOP}/SPECS/todero-native.spec"
cp "${RPM_TOP}/RPMS/${RPM_ARCH}/"*.rpm "${OUT_DIR}/rpm/"

echo "created ${OUT_DIR}/deb/todero-native_${VERSION}_${DEB_ARCH}.deb"
echo "created ${OUT_DIR}/rpm/todero-native-${RPM_VERSION}-1.${RPM_ARCH}.rpm"
