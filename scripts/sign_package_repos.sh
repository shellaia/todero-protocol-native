#!/usr/bin/env bash
set -euo pipefail

MODE="${1:?usage: sign_package_repos.sh <apt|yum> <repo_root>}"
REPO_ROOT="${2:?usage: sign_package_repos.sh <apt|yum> <repo_root>}"

if [[ ! -d "${REPO_ROOT}" ]]; then
  echo "repo root not found: ${REPO_ROOT}" >&2
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

case "${MODE}" in
  apt)
    release_file="${REPO_ROOT}/dists/stable/Release"
    if [[ ! -f "${release_file}" ]]; then
      echo "missing apt Release file: ${release_file}" >&2
      exit 1
    fi
    if [[ -n "${GPG_PASSPHRASE:-}" ]]; then
      gpg --batch --yes --pinentry-mode loopback \
        --passphrase "${GPG_PASSPHRASE}" \
        --local-user "${KEY_ID}" \
        --output "${release_file}.gpg" \
        --detach-sign "${release_file}"
      gpg --batch --yes --pinentry-mode loopback \
        --passphrase "${GPG_PASSPHRASE}" \
        --local-user "${KEY_ID}" \
        --output "${REPO_ROOT}/dists/stable/InRelease" \
        --clearsign "${release_file}"
    else
      gpg --batch --yes --local-user "${KEY_ID}" \
        --output "${release_file}.gpg" \
        --detach-sign "${release_file}"
      gpg --batch --yes --local-user "${KEY_ID}" \
        --output "${REPO_ROOT}/dists/stable/InRelease" \
        --clearsign "${release_file}"
    fi
    ;;
  yum)
    repomd="${REPO_ROOT}/repodata/repomd.xml"
    if [[ ! -f "${repomd}" ]]; then
      echo "missing yum repomd.xml file: ${repomd}" >&2
      exit 1
    fi
    if [[ -n "${GPG_PASSPHRASE:-}" ]]; then
      gpg --batch --yes --pinentry-mode loopback \
        --passphrase "${GPG_PASSPHRASE}" \
        --local-user "${KEY_ID}" \
        --armor --detach-sign \
        --output "${repomd}.asc" "${repomd}"
    else
      gpg --batch --yes --local-user "${KEY_ID}" \
        --armor --detach-sign \
        --output "${repomd}.asc" "${repomd}"
    fi
    ;;
  *)
    echo "unsupported mode: ${MODE}" >&2
    exit 1
    ;;
esac

echo "signed ${MODE} repository metadata with key ${KEY_ID}"
