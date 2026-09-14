#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$script_dir/.." && pwd)"
header_dir="$repo_root/apps/ios/Sources/RuneFFIHeaders/include"
binary="$repo_root/target/debug/rune-ffi-smoke"

cargo build -p rune-ffi --quiet

cc -std=c11 -Wall -Wextra -Werror \
  -I "$header_dir" \
  "$script_dir/ffi_smoke.c" \
  -L "$repo_root/target/debug" -lrune_ffi \
  -Wl,-rpath,"$repo_root/target/debug" \
  -o "$binary"

"$binary"
rm -f "$binary"
