#!/usr/bin/env bash
set -euo pipefail

project_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$project_root"

echo "==> compatibility matrix validation"
python3 scripts/validate_compatibility.py
python3 scripts/compatibility_runner.py --validate-only
python3 scripts/test_compatibility_runner.py

echo "==> shell script syntax"
bash -n scripts/ci.sh scripts/demo.sh scripts/bench.sh

echo "==> cargo fmt --check"
cargo fmt --all -- --check

echo "==> cargo clippy"
cargo clippy --workspace --all-targets --all-features -- -D warnings

echo "==> cargo test"
cargo test --workspace

echo "==> cargo build"
cargo build --workspace

if command -v swiftc >/dev/null 2>&1; then
  echo "==> swift package dump-package (source-only manifest check)"
  rune_swiftpm_scratch="$(mktemp -d /tmp/rune-swiftpm.XXXXXX)"
  trap 'find "$rune_swiftpm_scratch" -depth -delete' EXIT
  (cd apps/ios && swift package --scratch-path "$rune_swiftpm_scratch" dump-package >/dev/null)
  find "$rune_swiftpm_scratch" -depth -delete
  trap - EXIT

  echo "==> swiftc -parse (source-only Apple check)"
  swiftc -parse \
    apps/ios/Sources/RuneIOS/RuneCoreBridge.swift \
    apps/ios/Sources/RuneIOS/RuneClipboardBridge.swift \
    apps/ios/Sources/RuneIOS/RuneExternalFolderAccess.swift \
    apps/ios/Sources/RuneIOS/RuneNetworkBridge.swift \
    apps/ios/Sources/RuneIOS/RuneOpenBridge.swift \
    apps/ios/Sources/RuneIOS/RuneShortcuts.swift \
    apps/ios/Sources/RuneIOS/RuneANSIText.swift \
    apps/ios/Sources/RuneIOS/RuneTerminalView.swift \
    apps/ios/Sources/RuneIOS/RuneWorkspaceView.swift
fi

echo "Local Rune checks passed."
