#!/usr/bin/env bash
set -euo pipefail

project_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$project_root"

echo "==> cargo fmt --check"
cargo fmt --all -- --check

echo "==> cargo clippy"
cargo clippy --workspace --all-targets --all-features -- -D warnings

echo "==> cargo test"
cargo test --workspace

echo "==> cargo build"
cargo build --workspace

if command -v swiftc >/dev/null 2>&1; then
  echo "==> swiftc -parse (source-only Apple check)"
  swiftc -parse \
    apps/ios/Sources/RuneIOS/RuneCoreBridge.swift \
    apps/ios/Sources/RuneIOS/RuneTerminalView.swift
fi

echo "Local Rune checks passed."
