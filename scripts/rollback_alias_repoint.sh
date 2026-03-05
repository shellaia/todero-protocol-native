#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
usage:
  rollback_alias_repoint.sh --channel <apt|yum|brew> --bucket <bucket> --prefix <s3_prefix> --version <X.Y.Z> [--apply --yes]

behavior:
  - always validates snapshot completeness for the requested version
  - default mode is non-destructive dry-run (prints planned repoint action)
  - with --apply --yes, performs alias repoint by syncing snapshot -> alias
EOF
}

CHANNEL=""
BUCKET=""
PREFIX=""
VERSION=""
APPLY=0
YES=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    --channel) CHANNEL="${2:-}"; shift 2 ;;
    --bucket) BUCKET="${2:-}"; shift 2 ;;
    --prefix) PREFIX="${2:-}"; shift 2 ;;
    --version) VERSION="${2:-}"; shift 2 ;;
    --apply) APPLY=1; shift ;;
    --yes) YES=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) echo "unknown arg: $1" >&2; usage; exit 2 ;;
  esac
done

if [[ -z "${CHANNEL}" || -z "${BUCKET}" || -z "${PREFIX}" || -z "${VERSION}" ]]; then
  usage
  exit 2
fi

case "${CHANNEL}" in
  apt|yum|brew) ;;
  *) echo "unsupported channel: ${CHANNEL}" >&2; exit 2 ;;
esac

PREFIX="${PREFIX%/}"
SNAPSHOT_ROOT="s3://${BUCKET}/${PREFIX}/releases/${VERSION}/"
case "${CHANNEL}" in
  apt) ALIAS_ROOT="s3://${BUCKET}/${PREFIX}/channels/stable/" ;;
  yum|brew) ALIAS_ROOT="s3://${BUCKET}/${PREFIX}/" ;;
esac

head_uri() {
  local uri="$1"
  local no_scheme="${uri#s3://}"
  local bucket="${no_scheme%%/*}"
  local key="${no_scheme#*/}"
  aws s3api head-object --bucket "${bucket}" --key "${key}" >/dev/null
}

targets=(
  "linux-x86_64-gnu"
  "linux-aarch64-gnu"
  "darwin-aarch64"
)

# Base required snapshot objects for rollback candidate completeness.
for t in "${targets[@]}"; do
  archive="todero-native-${t}-${VERSION}.tar.gz"
  checksum="${archive}.sha256"
  head_uri "${SNAPSHOT_ROOT}${archive}"
  head_uri "${SNAPSHOT_ROOT}${checksum}"
done
head_uri "${SNAPSHOT_ROOT}todero-release-manifest-${VERSION}.json"

case "${CHANNEL}" in
  apt)
    head_uri "${SNAPSHOT_ROOT}dists/stable/InRelease"
    head_uri "${SNAPSHOT_ROOT}dists/stable/Release"
    head_uri "${SNAPSHOT_ROOT}dists/stable/Release.gpg"
    ;;
  yum)
    head_uri "${SNAPSHOT_ROOT}repodata/repomd.xml"
    head_uri "${SNAPSHOT_ROOT}repodata/repomd.xml.asc"
    ;;
  brew)
    head_uri "${SNAPSHOT_ROOT}todero-native.rb"
    ;;
esac

echo "snapshot completeness check passed: channel=${CHANNEL} version=${VERSION}"
echo "snapshot=${SNAPSHOT_ROOT}"
echo "alias=${ALIAS_ROOT}"

if [[ "${APPLY}" -eq 0 ]]; then
  echo "dry-run: would repoint alias by syncing snapshot -> alias"
  echo "command: aws s3 sync \"${SNAPSHOT_ROOT}\" \"${ALIAS_ROOT}\" --delete"
  exit 0
fi

if [[ "${YES}" -ne 1 ]]; then
  echo "--apply requires --yes" >&2
  exit 2
fi

aws s3 sync "${SNAPSHOT_ROOT}" "${ALIAS_ROOT}" --delete
echo "alias repoint applied: channel=${CHANNEL} version=${VERSION}"
