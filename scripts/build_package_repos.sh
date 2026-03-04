#!/usr/bin/env bash
set -euo pipefail

RELEASE_DIR="${1:-dist/release}"
APT_ROOT="${RELEASE_DIR}/repos/apt"
YUM_ROOT="${RELEASE_DIR}/repos/yum"
DEB_DIR="${RELEASE_DIR}/packages/deb"
RPM_DIR="${RELEASE_DIR}/packages/rpm"

if ! command -v dpkg-scanpackages >/dev/null 2>&1; then
  echo "dpkg-scanpackages is required" >&2
  exit 1
fi
if ! command -v createrepo_c >/dev/null 2>&1; then
  echo "createrepo_c is required" >&2
  exit 1
fi

rm -rf "${APT_ROOT}" "${YUM_ROOT}"
mkdir -p "${APT_ROOT}/pool/main/t/todero-native"
mkdir -p "${YUM_ROOT}/packages"

if compgen -G "${DEB_DIR}/*.deb" >/dev/null 2>&1; then
  cp "${DEB_DIR}/"*.deb "${APT_ROOT}/pool/main/t/todero-native/"
fi
if compgen -G "${RPM_DIR}/*.rpm" >/dev/null 2>&1; then
  cp "${RPM_DIR}/"*.rpm "${YUM_ROOT}/packages/"
fi

write_apt_arch() {
  local arch="$1"
  local out_dir="${APT_ROOT}/dists/stable/main/binary-${arch}"
  mkdir -p "${out_dir}"
  (
    cd "${APT_ROOT}"
    dpkg-scanpackages --arch "${arch}" pool /dev/null > "dists/stable/main/binary-${arch}/Packages"
  )
  gzip -9c "${out_dir}/Packages" > "${out_dir}/Packages.gz"
}

write_apt_arch "amd64"
write_apt_arch "arm64"

RELEASE_FILE="${APT_ROOT}/dists/stable/Release"
mkdir -p "$(dirname "${RELEASE_FILE}")"
{
  echo "Origin: Todero"
  echo "Label: Todero Native"
  echo "Suite: stable"
  echo "Codename: stable"
  echo "Date: $(LC_ALL=C date -Ru)"
  echo "Architectures: amd64 arm64"
  echo "Components: main"
  echo "Description: Todero native apt repository"
  echo "SHA256:"
  for f in \
    main/binary-amd64/Packages \
    main/binary-amd64/Packages.gz \
    main/binary-arm64/Packages \
    main/binary-arm64/Packages.gz; do
    if [[ -f "${APT_ROOT}/dists/stable/${f}" ]]; then
      sum="$(sha256sum "${APT_ROOT}/dists/stable/${f}" | awk '{print $1}')"
      size="$(wc -c < "${APT_ROOT}/dists/stable/${f}" | tr -d ' ')"
      printf " %s %16s %s\n" "${sum}" "${size}" "${f}"
    fi
  done
} > "${RELEASE_FILE}"

createrepo_c "${YUM_ROOT}"
if [[ -f "${YUM_ROOT}/repodata/repomd.xml" ]]; then
  cp "${YUM_ROOT}/repodata/repomd.xml" "${YUM_ROOT}/repodata/repomd.xml.unsigned"
fi

echo "generated apt metadata under ${APT_ROOT}"
echo "generated yum metadata under ${YUM_ROOT}"
