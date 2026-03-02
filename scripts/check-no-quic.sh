#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
if cargo tree --workspace | rg -i '\bquic\b|quinn|s2n-quic|msquic|quiche' >/dev/null; then
  echo 'QUIC-related dependency detected in v3 path'
  exit 1
fi
echo 'No QUIC-related dependencies detected.'
