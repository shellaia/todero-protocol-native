#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
FUZZ_DIR="$ROOT_DIR/v3-wire/fuzz"

if ! command -v cargo >/dev/null 2>&1; then
  echo "cargo not found; skipping fuzz smoke"
  exit 0
fi

if ! cargo fuzz --help >/dev/null 2>&1; then
  echo "cargo-fuzz not installed; skipping fuzz smoke"
  exit 0
fi

if ! rustup toolchain list | grep -q '^nightly'; then
  echo "nightly toolchain not installed; skipping fuzz smoke"
  exit 0
fi

cd "$FUZZ_DIR"
cargo +nightly fuzz run decode_frame -- -runs=2000
