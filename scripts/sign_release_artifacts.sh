#!/usr/bin/env bash
set -euo pipefail

ART_DIR="${1:-dist/release}"
SIG_SUFFIX="${2:-.asc}"
KEY_BASENAME="${3:-todero-native-repo}"

if [[ ! -d "${ART_DIR}" ]]; then
  echo "artifact directory not found: ${ART_DIR}" >&2
  exit 1
fi

if ! command -v gpg >/dev/null 2>&1; then
  echo "gpg is required but not found in PATH" >&2
  exit 1
fi

: "${GPG_PRIVATE_KEY:?GPG_PRIVATE_KEY is required}"

import_key() {
  local key_value="$1"
  if echo "${key_value}" | grep -q "BEGIN PGP"; then
    printf '%s\n' "${key_value}" | gpg --batch --import
  else
    printf '%s' "${key_value}" | base64 --decode | gpg --batch --import
  fi
}

tmp_gpg_home="$(mktemp -d)"
export GNUPGHOME="${tmp_gpg_home}"
trap 'rm -rf "${tmp_gpg_home}"' EXIT

cat > "${GNUPGHOME}/gpg.conf" <<'CFG'
batch
yes
pinentry-mode loopback
CFG

cat > "${GNUPGHOME}/gpg-agent.conf" <<'CFG'
allow-loopback-pinentry
CFG

import_key "${GPG_PRIVATE_KEY}"

KEY_ID="${GPG_KEY_ID:-}"
KEY_ID="$(printf '%s' "${KEY_ID}" | tr -d '[:space:]')"
if [[ -z "${KEY_ID}" ]]; then
  KEY_ID="$(gpg --batch --list-secret-keys --with-colons | awk -F: '/^sec:/{print $5; exit}')"
fi
if [[ -z "${KEY_ID}" ]]; then
  echo "unable to resolve signing key id from imported private key" >&2
  exit 1
fi

gpg --batch --yes --export "${KEY_ID}" > "${ART_DIR}/${KEY_BASENAME}.gpg"
gpg --batch --yes --armor --export "${KEY_ID}" > "${ART_DIR}/${KEY_BASENAME}.asc"

shopt -s nullglob
targets=(
  "${ART_DIR}"/*.tar.gz
  "${ART_DIR}"/*.sha256
  "${ART_DIR}"/*.json
  "${ART_DIR}"/*.rb
)

if [[ "${#targets[@]}" -eq 0 ]]; then
  echo "no release artifacts found to sign in ${ART_DIR}" >&2
  exit 1
fi

for f in "${targets[@]}"; do
  sig="${f}${SIG_SUFFIX}"
  if [[ -n "${GPG_PASSPHRASE:-}" ]]; then
    gpg --batch --yes --pinentry-mode loopback \
      --passphrase "${GPG_PASSPHRASE}" \
      --local-user "${KEY_ID}" \
      --armor --detach-sign \
      --output "${sig}" "${f}"
  else
    gpg --batch --yes \
      --local-user "${KEY_ID}" \
      --armor --detach-sign \
      --output "${sig}" "${f}"
  fi
done

echo "signed release artifacts with key ${KEY_ID}"
