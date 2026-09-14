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

echo "==> CLI metadata"
rune_cli_version="$(target/debug/rune-cli --version)"
test "$rune_cli_version" = "rune-cli 0.1.0"
target/debug/rune-cli --help | grep -F -- "usage: rune [--root PATH] [-c COMMAND | --script PATH]" >/dev/null
rune_cli_smoke_root="$(mktemp -d "${TMPDIR:-/tmp}/rune-cli-smoke.XXXXXX")"
target/debug/rune-cli --root "$rune_cli_smoke_root" -c \
  "mkdir -p 'folder name'; printf '%s\\n' 'printf script-ok' > 'folder name/script file.rune'" >/dev/null
test "$(target/debug/rune-cli --root "$rune_cli_smoke_root" --script "folder name/script file.rune")" = "script-ok"
if target/debug/rune-cli --root "$rune_cli_smoke_root" -c true --script \
  "folder name/script file.rune" >/dev/null 2>&1; then
  echo "rune: --command and --script conflict was accepted" >&2
  exit 1
fi
find "$rune_cli_smoke_root" -depth -delete

if command -v swiftc >/dev/null 2>&1; then
  echo "==> swift package dump-package (source-only manifest check)"
  rune_swiftpm_scratch="$(mktemp -d /tmp/rune-swiftpm.XXXXXX)"
  trap 'find "$rune_swiftpm_scratch" -depth -delete' EXIT
  (cd apps/ios && swift package --scratch-path "$rune_swiftpm_scratch" dump-package >/dev/null)
  echo "==> SwiftPM C header target"
  (cd apps/ios && swift build --target RuneFFIHeaders --scratch-path "$rune_swiftpm_scratch" >/dev/null)
  rune_swift_module_map="$(find "$rune_swiftpm_scratch" -path '*RuneFFIHeaders.build/module.modulemap' -print -quit)"
  test -n "$rune_swift_module_map"

  echo "==> swiftc -typecheck (source-only Apple boundary check)"
  swiftc -typecheck \
    -Xcc -fmodule-map-file="$rune_swift_module_map" \
    -Xcc -I -Xcc apps/ios/Sources/RuneFFIHeaders/include \
    apps/ios/Sources/RuneIOS/RuneCoreBridge.swift \
    apps/ios/Sources/RuneIOS/RuneClipboardBridge.swift \
    apps/ios/Sources/RuneIOS/RuneExternalFolderAccess.swift \
    apps/ios/Sources/RuneIOS/RuneNetworkBridge.swift \
    apps/ios/Sources/RuneIOS/RuneOpenBridge.swift \
    apps/ios/Sources/RuneIOS/RuneShortcuts.swift \
    apps/ios/Sources/RuneIOS/RuneANSIText.swift \
    apps/ios/Sources/RuneIOS/RuneTerminalView.swift \
    apps/ios/Sources/RuneIOS/RuneWorkspaceView.swift
  find "$rune_swiftpm_scratch" -depth -delete
  trap - EXIT
fi

echo "Local Rune checks passed."
