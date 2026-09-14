#!/usr/bin/env bash
set -euo pipefail

project_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$project_root"

release_version="${1:-}"
output_directory="${2:-dist/release}"
if [[ -z "$release_version" || ! "$release_version" =~ ^[0-9]+\.[0-9]+\.[0-9]+([.-][0-9A-Za-z.-]+)?$ ]]; then
  echo "usage: $0 VERSION [OUTPUT_DIRECTORY]" >&2
  echo "example: $0 0.1.0-alpha.7" >&2
  exit 2
fi

if [[ "$(uname -s)" != "Darwin" || "$(uname -m)" != "arm64" ]]; then
  echo "rune: this portable release packager currently supports macOS arm64 only" >&2
  exit 1
fi

echo "==> cargo build --release -p rune-cli"
cargo build --release -p rune-cli

package_name="rune-cli-v${release_version}-macos-arm64"
staging_directory="$(mktemp -d "${TMPDIR:-/tmp}/rune-package.XXXXXX")"
trap 'rm -rf "$staging_directory"' EXIT
mkdir -p "$staging_directory/$package_name" "$output_directory"
cp target/release/rune-cli "$staging_directory/$package_name/rune-cli"
chmod 755 "$staging_directory/$package_name/rune-cli"
touch -t 200001010000 "$staging_directory/$package_name" "$staging_directory/$package_name/rune-cli"

archive="$output_directory/$package_name.tar.gz"
checksum_file="$output_directory/SHA256SUMS-v${release_version}.txt"
tar -C "$staging_directory" --format ustar --numeric-owner -cf - "$package_name" \
  | gzip -n > "$archive"
(cd "$output_directory" && shasum -a 256 "$(basename "$archive")" > "$(basename "$checksum_file")")

echo "Packaged: $archive"
echo "Checksum: $checksum_file"
shasum -a 256 "$archive"
