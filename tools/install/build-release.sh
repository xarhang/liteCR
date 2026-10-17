#!/usr/bin/env bash
set -euo pipefail
umask 077
export LC_ALL=C
export TZ=UTC

fail() { printf 'liteCR release build failed [%s]\n' "$1" >&2; exit 1; }
root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd -P)"
output_dir=""
while (($#)); do
  case "$1" in
    --output-dir)
      (($# >= 2)) || fail USAGE
      output_dir="$2"
      shift 2
      ;;
    *) fail USAGE ;;
  esac
done
[[ -n "$output_dir" && "$output_dir" == /* ]] || fail ABSOLUTE_OUTPUT_DIR_REQUIRED
[[ "${SOURCE_DATE_EPOCH:-}" =~ ^[0-9]+$ ]] || fail SOURCE_DATE_EPOCH_REQUIRED
[[ "$(uname -s)" == Linux && "$(uname -m)" == x86_64 ]] || fail UNSUPPORTED_BUILD_HOST
host_target="$(rustc -vV | sed -n 's/^host: //p')"
[[ "$host_target" == x86_64-unknown-linux-gnu ]] || fail UNSUPPORTED_BUILD_TARGET
for command in cargo rustc node npm tar gzip sha256sum find sort xargs cp install; do
  command -v "$command" >/dev/null 2>&1 || fail "MISSING_$command"
done

# Record a Git revision only when it identifies the exact clean checkout used
# by this build. Local source trees without Git metadata, dirty trees, and
# unreadable Git state remain buildable, but the package explicitly records
# that source revision provenance is unavailable.
source_revision=""
source_revision_status="no_git_metadata"
if command -v git >/dev/null 2>&1 && git -C "$root" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
  source_revision_status="git_revision_unavailable"
  if candidate_revision="$(git -C "$root" rev-parse --verify 'HEAD^{commit}' 2>/dev/null)" \
    && [[ "$candidate_revision" =~ ^([0-9a-f]{40}|[0-9a-f]{64})$ ]]; then
    if git_status="$(git -C "$root" status --porcelain --untracked-files=all 2>/dev/null)"; then
      if [[ -z "$git_status" ]]; then
        source_revision="$candidate_revision"
        source_revision_status="clean_git_worktree"
      else
        source_revision_status="dirty_git_worktree"
      fi
    else
      source_revision_status="git_status_unavailable"
    fi
  fi
fi
export LITECR_SOURCE_REVISION="$source_revision"
export LITECR_SOURCE_REVISION_STATUS="$source_revision_status"

temp_dir="$(mktemp -d)"
cleanup() { rm -rf -- "$temp_dir"; }
trap cleanup EXIT
web_source="$temp_dir/web-source"
web_output="$temp_dir/web-dist"
release_id=""
mkdir -m 700 -- "$web_source"
# Preserve the workspace-relative package imports while isolating npm ci from workspace node_modules.
tar -C "$root" \
  --exclude='apps/web/node_modules' --exclude='apps/web/dist' --exclude='apps/web/test-results' \
  -cf - apps/web packages | tar -C "$web_source" -xf -
web_project="$web_source/apps/web"
(cd "$web_project" && npm ci --no-audit --no-fund && npm run build)
[[ -f "$web_project/dist/index.html" ]] || fail WEB_BUILD_MISSING_INDEX
mv -- "$web_project/dist" "$web_output"

metadata="$temp_dir/cargo-metadata.json"
(cd "$root" && cargo metadata --locked --format-version 1 > "$metadata") || fail CARGO_METADATA
version="$(node -e 'const d=require(process.argv[1]);const p=d.packages.find(x=>x.name==="litecr-server");if(!p)process.exit(1);process.stdout.write(p.version)' "$metadata")" || fail SERVER_PACKAGE_METADATA
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?$ ]] || fail INVALID_RELEASE_VERSION
release_id="litecr-${version}-linux-x86_64"
stage="$temp_dir/$release_id"
mkdir -m 700 -p -- "$stage/bin" "$stage/share/migrations" "$stage/share/tools/migration" "$stage/share/tools/operations" "$stage/share/templates" "$stage/share/metadata" "$stage/share/docs"

(cd "$root" && LITECR_WEB_DIST="$web_output" cargo build --locked --release --target "$host_target" -p litecr-server) || fail CARGO_RELEASE_BUILD
cargo_target_dir="${CARGO_TARGET_DIR:-$root/target}"
[[ "$cargo_target_dir" == /* ]] || cargo_target_dir="$root/$cargo_target_dir"
binary="$cargo_target_dir/$host_target/release/litecr"
[[ -x "$binary" && ! -L "$binary" ]] || fail RELEASE_BINARY_MISSING
binary_metadata="$("$binary" version --json)" || fail RELEASE_BINARY_VERSION
binary_version="$(node -e 'const d=JSON.parse(process.argv[1]);process.stdout.write(d.application_version)' "$binary_metadata")" || fail RELEASE_BINARY_VERSION
binary_schema="$(node -e 'const d=JSON.parse(process.argv[1]);process.stdout.write(String(d.schema_version))' "$binary_metadata")" || fail RELEASE_BINARY_SCHEMA
source_schema="$(find "$root/crates/infrastructure/migrations" -maxdepth 1 -type f -name '[0-9][0-9][0-9][0-9]_*.sql' -printf '%f\n' | LC_ALL=C sort | tail -n 1 | cut -d_ -f1 | sed 's/^0*//')"
[[ "$binary_version" == "$version" && -n "$source_schema" && "$binary_schema" == "$source_schema" ]] || fail RELEASE_BINARY_SOURCE_MISMATCH
install -m 755 -- "$binary" "$stage/bin/litecr"
cp -a -- "$root/crates/infrastructure/migrations/." "$stage/share/migrations/"
cp -- "$root/tools/migration/litecr_migrate.py" "$stage/share/tools/migration/"
install -m 0755 -- "$root/tools/operations/grant-runtime-database-access.sh" "$stage/share/tools/operations/"
install -m 0644 -- "$root/tools/operations/grant-runtime-database-access.sql" "$stage/share/tools/operations/"
install -m 0755 -- "$root/tools/operations/grant-worker-database-access.sh" "$stage/share/tools/operations/"
install -m 0755 -- "$root/tools/operations/activate-pending-release.sh" "$stage/share/tools/operations/"
install -m 0755 -- "$root/tools/operations/backup-postgres.sh" "$root/tools/operations/restore-postgres-isolated.sh" "$stage/share/tools/operations/"
install -m 0644 -- "$root/tools/operations/grant-worker-database-access.sql" "$stage/share/tools/operations/"
cp -- "$root/infra/release/litecr-api.service" "$root/infra/release/litecr.env.example" \
  "$root/infra/release/litecr-migrate.service" "$root/infra/release/litecr-activate.service" \
  "$root/infra/release/litecr-migration.env.example" \
  "$root/infra/release/litecr-worker.service" "$root/infra/release/litecr-worker.env.example" \
  "$root/infra/release/Caddyfile.example" "$stage/share/templates/"
cp -- "$root/tools/install/install.sh" "$stage/install.sh"
cp -- "$root/LICENSE" "$stage/LICENSE"
cmp -s -- "$root/LICENSE" "$stage/LICENSE" || fail LICENSE_COPY
cp -- "$root/docs/runbooks/backup-restore.md" "$root/docs/runbooks/upgrade-rollback.md" \
  "$root/docs/runbooks/redacted-diagnostics.md" "$root/docs/runbooks/migration-operator.md" \
  "$root/docs/runbooks/database-roles.md" "$stage/share/docs/"

"$stage/bin/litecr" version --json > "$stage/share/metadata/version.json" || fail VERSION_COMMAND
node "$root/tools/install/write-release-metadata.mjs" \
  "$metadata" "$root/Cargo.lock" "$root/apps/web/package-lock.json" "$stage/share/metadata/version.json" \
  "$stage/share/metadata/cargo-dependencies.json" "$stage/share/metadata/npm-licenses.json" \
  "$stage/share/metadata/THIRD-PARTY-NOTICES.md" "$stage/share/metadata/release.json" \
  "$stage/share/metadata/sbom.spdx.json" \
  || fail LICENSE_METADATA
npm_cache="$(cd -- "$web_project" && npm config get cache)"
node "$root/tools/install/collect-third-party-texts.mjs" \
  "$metadata" "$root/Cargo.lock" "$root/apps/web/package-lock.json" \
  "${CARGO_HOME:-$HOME/.cargo}" "$web_project/node_modules" "$npm_cache" \
  "$stage/share/metadata/THIRD-PARTY-TEXTS" \
  || fail THIRD_PARTY_TEXT_COLLECTION
printf 'platform=linux-x86_64\n' > "$stage/share/metadata/platform.txt"
printf 'Rust toolchain: %s\nNode: %s\nnpm: %s\nSOURCE_DATE_EPOCH: %s\n' \
  "$(rustc --version)" "$(node --version)" "$(npm --version)" "$SOURCE_DATE_EPOCH" \
  > "$stage/share/metadata/build-tools.txt"
cp -- "$root/tools/install/INSTALL.md" "$stage/INSTALL.md"
find "$stage" -type d -exec chmod 0755 -- {} +
find "$stage" -type f -exec chmod 0644 -- {} +
chmod 0755 -- "$stage/bin/litecr" "$stage/install.sh" \
  "$stage/share/tools/operations/grant-runtime-database-access.sh" \
  "$stage/share/tools/operations/grant-worker-database-access.sh" \
  "$stage/share/tools/operations/activate-pending-release.sh" \
  "$stage/share/tools/operations/backup-postgres.sh" \
  "$stage/share/tools/operations/restore-postgres-isolated.sh"
(cd "$stage" && find . -type f ! -name SHA256SUMS -print0 | sort -z | xargs -0 sha256sum > SHA256SUMS)
chmod 0644 -- "$stage/SHA256SUMS"

if [[ ! -d "$output_dir" ]]; then install -d -m 0755 -- "$output_dir"; fi
final_dir="$output_dir/$release_id"
archive="$output_dir/$release_id.tar.gz"
[[ ! -e "$final_dir" && ! -e "$archive" && ! -e "$archive.sha256" ]] || fail RELEASE_OUTPUT_EXISTS
source_archive=""; staged_source_archive=""
if [[ "$source_revision_status" == clean_git_worktree ]]; then
  source_name="litecr-${version}-source-${source_revision:0:12}.tar.gz"
  source_archive="$output_dir/$source_name"
  [[ ! -e "$source_archive" && ! -L "$source_archive" \
    && ! -e "$source_archive.sha256" && ! -L "$source_archive.sha256" ]] || fail SOURCE_OUTPUT_EXISTS
  staged_source_archive="$temp_dir/$source_name"
fi
# Build the canonical archive while the staged tree still resides on Linux's
# filesystem. Moving it to /mnt/d first loses Unix mode bits on WSL/DrvFs.
staged_archive="$temp_dir/$release_id.tar.gz"
staged_checksum="$temp_dir/$release_id.tar.gz.sha256"
tar --sort=name --mtime='@0' --owner=0 --group=0 --numeric-owner -C "$temp_dir" -cf - "$release_id" \
  | gzip -n > "$staged_archive"
tar --list --verbose --file "$staged_archive" | awk -v root="$release_id" '
  {
    mode = $1
    path = $6
    if (mode ~ /^d/) expected = "drwxr-xr-x"
    else if (path == root "/bin/litecr" || path == root "/install.sh" \
      || path == root "/share/tools/operations/grant-runtime-database-access.sh" \
      || path == root "/share/tools/operations/grant-worker-database-access.sh" \
      || path == root "/share/tools/operations/activate-pending-release.sh" \
      || path == root "/share/tools/operations/backup-postgres.sh" \
      || path == root "/share/tools/operations/restore-postgres-isolated.sh") expected = "-rwxr-xr-x"
    else expected = "-rw-r--r--"
    if (mode != expected) {
      printf "release archive mode mismatch: %s has %s, expected %s\n", path, mode, expected > "/dev/stderr"
      failed = 1
    }
  }
  END { if (NR == 0) failed = 1; exit failed }
' || fail ARCHIVE_MODE_VALIDATION
(cd "$temp_dir" && sha256sum "$release_id.tar.gz" > "$release_id.tar.gz.sha256")
if [[ -n "$staged_source_archive" ]]; then
  bash "$root/tools/install/package-source-snapshot.sh" \
    --repo "$root" --revision "$source_revision" --version "$version" --output "$staged_source_archive" \
    || fail SOURCE_SNAPSHOT
fi
mv -- "$staged_archive" "$archive"
mv -- "$stage" "$final_dir"
mv -- "$staged_checksum" "$archive.sha256"
chmod 644 -- "$archive" "$archive.sha256"
printf 'Release built: %s\nSHA-256: %s\n' "$archive" "$(awk '{print $1}' "$archive.sha256")"
if [[ -n "$source_archive" ]]; then
  mv -- "$staged_source_archive" "$source_archive"
  mv -- "$staged_source_archive.sha256" "$source_archive.sha256"
  chmod 644 -- "$source_archive" "$source_archive.sha256"
  printf 'Source snapshot: %s\nSHA-256: %s\n' "$source_archive" "$(awk '{print $1}' "$source_archive.sha256")"
fi
