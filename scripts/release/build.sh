#!/usr/bin/env bash
# Native, fresh, unprivileged build. Never reuse PR caches here.
set -euo pipefail
: "${VERSION:?}" "${TARGET:?}" "${RUNNER_TEMP:?}" "${GITHUB_WORKSPACE:?}"
python3 scripts/ci/run_fast.py
export CARGO_NET_OFFLINE=true
export RUSTFLAGS="--remap-path-prefix=$GITHUB_WORKSPACE=/src --remap-path-prefix=$HOME=/builder"
cargo build --frozen --release -p rdy-cli
cargo run --frozen --release -p rdy-qa --bin qa-release-package -- \
  --binary "$GITHUB_WORKSPACE/target/release/rdy" --version "$VERSION" \
  --target "$TARGET" --out "$RUNNER_TEMP/native"
python3 scripts/release/control.py record --version "$VERSION" --target "$TARGET" --directory "$RUNNER_TEMP/native"
git diff --exit-code
