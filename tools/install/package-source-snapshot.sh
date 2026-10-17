#!/usr/bin/env bash
set -euo pipefail
umask 077
export LC_ALL=C
fail() { printf 'liteCR source snapshot failed [%s]\n' "$1" >&2; exit 1; }

repo=""; revision=""; version=""; output=""
while (($#)); do
  case "$1" in
    --repo) (($# >= 2)) || fail USAGE; repo="$2"; shift 2 ;;
    --revision) (($# >= 2)) || fail USAGE; revision="$2"; shift 2 ;;
    --version) (($# >= 2)) || fail USAGE; version="$2"; shift 2 ;;
    --output) (($# >= 2)) || fail USAGE; output="$2"; shift 2 ;;
    *) fail USAGE ;;
  esac
done
[[ "$repo" == /* && -d "$repo" && "$output" == /* && -n "$revision" && -n "$version" ]] || fail ARGUMENTS
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9A-Za-z.+-]+$ ]] || fail INVALID_VERSION
[[ "$revision" =~ ^([0-9a-f]{40}|[0-9a-f]{64})$ ]] || fail INVALID_REVISION
repo="$(cd -- "$repo" && pwd -P)"
git -C "$repo" rev-parse --is-inside-work-tree >/dev/null 2>&1 || fail GIT_REQUIRED
[[ "$(git -C "$repo" rev-parse --show-toplevel)" == "$repo" ]] || fail REPOSITORY_ROOT_MISMATCH
[[ "$(git -C "$repo" rev-parse --verify 'HEAD^{commit}')" == "$revision" ]] || fail HEAD_REVISION_MISMATCH
[[ -z "$(git -C "$repo" status --porcelain --untracked-files=all)" ]] || fail DIRTY_WORKTREE

short_revision="${revision:0:12}"
archive_name="litecr-${version}-source-${short_revision}.tar.gz"
[[ "$(basename -- "$output")" == "$archive_name" ]] || fail OUTPUT_NAME_MISMATCH
sidecar="$output.sha256"
[[ ! -e "$output" && ! -L "$output" && ! -e "$sidecar" && ! -L "$sidecar" ]] || fail OUTPUT_EXISTS
output_dir="$(dirname -- "$output")"
[[ -d "$output_dir" ]] || fail OUTPUT_DIRECTORY_MISSING

temp_archive=""; temp_sidecar=""
cleanup() { rm -f -- "$temp_archive" "$temp_sidecar"; }
trap cleanup EXIT
temp_archive="$(mktemp "$output_dir/.${archive_name}.XXXXXXXX")" || fail TEMP_ARCHIVE
temp_sidecar="$(mktemp "$output_dir/.${archive_name}.sha256.XXXXXXXX")" || fail TEMP_SIDECAR
prefix="litecr-${version}-source-${short_revision}/"
git -C "$repo" archive --format=tar --prefix="$prefix" "$revision" | gzip -n > "$temp_archive" || fail GIT_ARCHIVE
tar -tzf "$temp_archive" >/dev/null || fail SOURCE_ARCHIVE_INVALID
for required in Cargo.toml Cargo.lock LICENSE apps/web/package.json apps/web/package-lock.json \
  crates/domain/Cargo.toml crates/infrastructure/migrations/0001_init.sql tools/install/build-release.sh \
  tools/install/collect-third-party-texts.mjs tools/migration/litecr_migrate.py; do
  tar -tzf "$temp_archive" | grep -Fx -- "$prefix$required" >/dev/null || fail "SOURCE_FILE_MISSING_$required"
done
digest="$(sha256sum -- "$temp_archive" | awk '{print $1}')" || fail SOURCE_CHECKSUM
printf '%s  %s\n' "$digest" "$archive_name" > "$temp_sidecar" || fail SOURCE_CHECKSUM
mv -- "$temp_archive" "$output"
mv -- "$temp_sidecar" "$sidecar"
trap - EXIT
printf 'Source snapshot built: %s\nSHA-256: %s\n' "$output" "$(awk '{print $1}' "$sidecar")"
