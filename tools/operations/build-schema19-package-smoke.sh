#!/usr/bin/env bash
set -euo pipefail
umask 077

fail() { printf 'liteCR schema-19 smoke build failed [%s]\n' "$1" >&2; exit 1; }
repo="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd -P)"
[[ $# -eq 1 && "$1" == /* ]] || fail ABSOLUTE_OUTPUT_DIRECTORY_REQUIRED
output_dir="$1"
node_version=22.23.3
[[ "$(uname -s)" == Linux && "$(uname -m)" == x86_64 ]] || fail LINUX_X86_64_REQUIRED
for command in curl tar sha256sum cargo rustc; do command -v "$command" >/dev/null 2>&1 || fail "MISSING_$command"; done
[[ ! -e "$output_dir" && ! -L "$output_dir" ]] || fail OUTPUT_ALREADY_EXISTS

runtime="$(mktemp -d /tmp/litecr-node22-schema19.XXXXXXXX)"
cleanup() { rm -rf -- "$runtime"; }
trap cleanup EXIT
curl --fail --silent --show-error --location "https://nodejs.org/dist/v${node_version}/SHASUMS256.txt" -o "$runtime/SHASUMS256.txt" || fail NODE_CHECKSUM_DOWNLOAD
archive_name="node-v${node_version}-linux-x64.tar.xz"
expected="$(awk -v name="$archive_name" '$2 == name {print $1}' "$runtime/SHASUMS256.txt")"
[[ "$expected" =~ ^[0-9a-f]{64}$ ]] || fail NODE_CHECKSUM_ENTRY
curl --fail --silent --show-error --location "https://nodejs.org/dist/v${node_version}/${archive_name}" -o "$runtime/$archive_name" || fail NODE_ARCHIVE_DOWNLOAD
printf '%s  %s\n' "$expected" "$runtime/$archive_name" | sha256sum --check --status || fail NODE_ARCHIVE_CHECKSUM
tar -xJf "$runtime/$archive_name" -C "$runtime" || fail NODE_ARCHIVE_EXTRACTION
export PATH="$runtime/node-v${node_version}-linux-x64/bin:$PATH"
export SOURCE_DATE_EPOCH=1791590400
printf 'Verified Node.js: %s (%s)\n' "$(node --version)" "$expected"
bash "$repo/tools/install/build-release.sh" --output-dir "$output_dir"
