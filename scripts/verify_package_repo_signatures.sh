#!/usr/bin/env bash
set -euo pipefail

MODE="${1:?usage: verify_package_repo_signatures.sh <apt|yum> <repo_root> <public_key_asc>}"
REPO_ROOT="${2:?usage: verify_package_repo_signatures.sh <apt|yum> <repo_root> <public_key_asc>}"
PUB_ASC="${3:?usage: verify_package_repo_signatures.sh <apt|yum> <repo_root> <public_key_asc>}"

if [[ ! -d "${REPO_ROOT}" ]]; then
  echo "repo root not found: ${REPO_ROOT}" >&2
  exit 1
fi
if [[ ! -f "${PUB_ASC}" ]]; then
  echo "public key not found: ${PUB_ASC}" >&2
  exit 1
fi
if ! command -v gpg >/dev/null 2>&1; then
  echo "gpg is required but not found in PATH" >&2
  exit 1
fi

tmp_gpg_home="$(mktemp -d)"
export GNUPGHOME="${tmp_gpg_home}"
trap 'rm -rf "${tmp_gpg_home}"' EXIT

gpg --batch --import "${PUB_ASC}" >/dev/null 2>&1

case "${MODE}" in
  apt)
    release_file="${REPO_ROOT}/dists/stable/Release"
    release_gpg="${REPO_ROOT}/dists/stable/Release.gpg"
    inrelease="${REPO_ROOT}/dists/stable/InRelease"
    for f in "${release_file}" "${release_gpg}" "${inrelease}"; do
      if [[ ! -f "${f}" ]]; then
        echo "missing apt metadata file: ${f}" >&2
        exit 1
      fi
    done
    if ! gpg --batch --verify "${release_gpg}" "${release_file}" >/tmp/verify-apt-release.log 2>&1; then
      echo "apt detached signature verification failed: ${release_gpg}" >&2
      cat /tmp/verify-apt-release.log >&2 || true
      exit 2
    fi
    # InRelease is clear-signed; verify without detached payload argument.
    if ! gpg --batch --verify "${inrelease}" >/tmp/verify-apt-inrelease.log 2>&1; then
      echo "apt clear-signed verification failed: ${inrelease}" >&2
      cat /tmp/verify-apt-inrelease.log >&2 || true
      exit 2
    fi
    ;;
  yum)
    repomd="${REPO_ROOT}/repodata/repomd.xml"
    repomd_asc="${REPO_ROOT}/repodata/repomd.xml.asc"
    for f in "${repomd}" "${repomd_asc}"; do
      if [[ ! -f "${f}" ]]; then
        echo "missing yum metadata file: ${f}" >&2
        exit 1
      fi
    done
    if ! gpg --batch --verify "${repomd_asc}" "${repomd}" >/tmp/verify-yum-repomd.log 2>&1; then
      echo "yum detached signature verification failed: ${repomd_asc}" >&2
      cat /tmp/verify-yum-repomd.log >&2 || true
      exit 2
    fi
    ;;
  *)
    echo "unsupported mode: ${MODE}" >&2
    exit 1
    ;;
esac

echo "verified ${MODE} repository signatures"
