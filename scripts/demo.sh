#!/usr/bin/env bash
set -euo pipefail

project_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
demo_root="$(mktemp -d "${TMPDIR:-/tmp}/rune-demo.XXXXXX")"
trap 'rm -rf "$demo_root"' EXIT

cd "$project_root"

echo "==> create and persist a real Rune workspace"
cargo run --quiet -p rune-cli -- --root "$demo_root" -c \
  "mkdir -p project/src; echo 'fn main() {}' > project/src/main.rs; cp -r project project-copy; config set history-limit 20"

echo "==> restore the same workspace in a new process"
cargo run --quiet -p rune-cli -- --root "$demo_root" -c \
  "pwd; stat project-copy/src/main.rs; du project-copy; mv project-copy project-moved; history 3"

echo "Local Rune demo passed."
