#!/usr/bin/env bash
set -euo pipefail

project_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$project_root"

if [[ ! -x /usr/bin/time ]]; then
  echo "rune benchmark: /usr/bin/time is required" >&2
  exit 2
fi

echo "==> build the measured CLI"
cargo build --quiet -p rune-cli
rune_binary="$project_root/target/debug/rune-cli"

benchmark_root="$(mktemp -d "${TMPDIR:-/tmp}/rune-bench.XXXXXX")"
cleanup() {
  find "$benchmark_root" -depth -delete
}
trap cleanup EXIT

run_case() {
  local label="$1"
  local command="$2"
  echo "==> $label"
  /usr/bin/time -p "$rune_binary" --root "$benchmark_root" -c "$command" >/dev/null
}

echo "Rune local benchmark (warm build, temporary confined root)"
run_case "CLI startup" "true"
run_case "filesystem pipeline" "mkdir -p bench/src; printf 'fn main() {}\\n' > bench/src/main.rs; find bench -type f -name '*.rs' | wc -l"
run_case "bounded tree rendering" "tree bench"
