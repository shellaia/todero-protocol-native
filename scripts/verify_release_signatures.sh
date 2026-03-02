#!/usr/bin/env bash
set -euo pipefail

ART_DIR="${1:-dist/release}"
SIG_SUFFIX="${2:-.asc}"
PUB_ASC="${3:-${ART_DIR}/todero-native-repo.asc}"

if [[ ! -d "${ART_DIR}" ]]; then
  echo "artifact directory not found: ${ART_DIR}" >&2
  exit 1
fi

if ! command -v gpg >/dev/null 2>&1; then
  echo "gpg is required but not found in PATH" >&2
  exit 1
fi

if [[ ! -f "${PUB_ASC}" ]]; then
  echo "missing exported public key: ${PUB_ASC}" >&2
  exit 1
fi

tmp_gpg_home="$(mktemp -d)"
export GNUPGHOME="${tmp_gpg_home}"
trap 'rm -rf "${tmp_gpg_home}"' EXIT

gpg --batch --import "${PUB_ASC}" >/dev/null 2>&1

shopt -s nullglob
targets=(
  "${ART_DIR}"/*.tar.gz
  "${ART_DIR}"/*.sha256
  "${ART_DIR}"/*.json
  "${ART_DIR}"/*.rb
)

if [[ "${#targets[@]}" -eq 0 ]]; then
  echo "no release artifacts found to verify in ${ART_DIR}" >&2
  exit 1
fi

for f in "${targets[@]}"; do
  sig="${f}${SIG_SUFFIX}"
  if [[ ! -f "${sig}" ]]; then
    echo "missing signature file: ${sig}" >&2
    exit 1
  fi
  gpg --batch --verify "${sig}" "${f}" >/dev/null 2>&1
done

echo "signature verification passed for release artifacts"
