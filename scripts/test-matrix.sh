#!/usr/bin/env bash
set -euo pipefail
cargo test --workspace
cargo test --workspace --all-features
cargo test -p v3-crypto --no-default-features --features dtls-backend
cargo test -p v3-nat --no-default-features --features ice,turn
cargo test -p v3-store --no-default-features --features redis-store
./scripts/check-no-quic.sh
./scripts/fuzz-smoke.sh
