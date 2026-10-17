#!/usr/bin/env bash
set -euo pipefail
umask 077
fail() { printf 'liteCR installer stopped [%s]\n' "$1" >&2; exit 1; }
double_quote='"'; single_quote="'"
[[ "$(id -u)" == 0 ]] || fail ROOT_REQUIRED

bundle=""; checksum=""; runtime_env=""; migration_env=""; backup_verified=0
while (($#)); do
  case "$1" in
    --bundle) (($# >= 2)) || fail USAGE; bundle="$2"; shift 2 ;;
    --checksum) (($# >= 2)) || fail USAGE; checksum="$2"; shift 2 ;;
    --runtime-env-file) (($# >= 2)) || fail USAGE; runtime_env="$2"; shift 2 ;;
    --migration-env-file) (($# >= 2)) || fail USAGE; migration_env="$2"; shift 2 ;;
    --backup-verified) backup_verified=1; shift ;;
    --dry-run) dry_run=1; shift ;;
    *) fail USAGE ;;
  esac
done
[[ "$bundle" == /* && "$checksum" == /* && -f "$bundle" && ! -L "$bundle" && -f "$checksum" && ! -L "$checksum" ]] || fail BUNDLE_INPUT
[[ "$runtime_env" == /* && "$migration_env" == /* && -f "$runtime_env" && ! -L "$runtime_env" && -f "$migration_env" && ! -L "$migration_env" ]] || fail ENVIRONMENT_FILES
for command in uname stat awk tar sha256sum cmp find df getent id useradd groupadd install ln mv readlink systemctl curl mktemp grep chown chmod cp flock mkdir; do
  command -v "$command" >/dev/null 2>&1 || fail MISSING_REQUIRED_COMMAND
done
[[ "$(uname -s)" == Linux && "$(uname -m)" == x86_64 ]] || fail UNSUPPORTED_PLATFORM
systemctl --version >/dev/null 2>&1 || fail SYSTEMD_REQUIRED
if [[ ( -e /opt/litecr/current || -L /opt/litecr/current ) && "$backup_verified" != 1 ]]; then fail BACKUP_CONFIRMATION_REQUIRED; fi

protected_input() {
  local file="$1" owner mode
  [[ -f "$file" && ! -L "$file" ]] || fail PROTECTED_FILE_NOT_REGULAR
  owner="$(stat -c '%u' -- "$file")"; mode="$(stat -c '%a' -- "$file")"
  (( owner == 0 )) || fail PROTECTED_FILE_OWNER
  [[ "$mode" == 600 || "$mode" == 640 ]] || fail PROTECTED_FILE_MODE
}
protected_input "$runtime_env"; protected_input "$migration_env"
protected_artifact() {
  local file="$1" owner mode
  [[ -f "$file" && ! -L "$file" ]] || fail BUNDLE_FILE_NOT_REGULAR
  owner="$(stat -c '%u' -- "$file")"; mode="$(stat -c '%a' -- "$file")"
  (( owner == 0 )) || fail BUNDLE_FILE_OWNER
  (( (8#$mode & 022) == 0 )) || fail BUNDLE_FILE_WRITABLE
}
protected_artifact "$bundle"; protected_artifact "$checksum"

# Parse allowlisted key=value pairs as data; never source or evaluate an environment file.
declare -A allowed=([LITECR_DATABASE_URL]=1 [LITECR_BIND]=1 [LITECR_DATABASE_MAX_CONNECTIONS]=1 [LITECR_ENV]=1 [LITECR_OIDC_ISSUER_URL]=1 [LITECR_OIDC_CLIENT_ID]=1 [LITECR_OIDC_CLIENT_SECRET]=1 [LITECR_OIDC_REDIRECT_URI]=1 [LITECR_WEB_ORIGIN]=1 [LITECR_COOKIE_SECURE]=1 [LITECR_FHIR_CLIENT_BINDINGS]=1 [LITECR_SESSION_LIFETIME_SECONDS]=1 [LITECR_OIDC_FLOW_LIFETIME_SECONDS]=1 [LITECR_SESSION_IDLE_SECONDS]=1 [LITECR_MATCHING_PARALLELISM]=1)
declare -A seen=()
while IFS= read -r line || [[ -n "$line" ]]; do
  [[ -z "$line" || "$line" == \#* ]] && continue
  [[ "$line" == *=* && "$line" != *$'\r'* ]] || fail RUNTIME_ENV_SYNTAX
  key="${line%%=*}"; value="${line#*=}"
  [[ -n "${allowed[$key]+present}" && -z "${seen[$key]+present}" ]] || fail RUNTIME_ENV_KEY
  if [[ "$key" != LITECR_FHIR_CLIENT_BINDINGS ]]; then
    [[ "$value" != "$double_quote"* && "$value" != "$single_quote"* ]] || fail RUNTIME_ENV_QUOTED_VALUE
  fi
  seen[$key]=1
  printf -v "$key" '%s' "$value"
  if [[ "$key" == LITECR_DATABASE_URL || "$key" == LITECR_DATABASE_MAX_CONNECTIONS ]]; then export "$key"; fi
done < "$runtime_env"
[[ -n "${LITECR_DATABASE_URL:-}" && "${LITECR_ENV:-production}" == production ]] || fail RUNTIME_CONFIG_REQUIRED
[[ "${LITECR_COOKIE_SECURE:-true}" == true && "${LITECR_OIDC_ISSUER_URL:-}" == https://* && "${LITECR_WEB_ORIGIN:-}" == https://* ]] || fail HTTPS_AUTH_REQUIRED
[[ -n "${LITECR_OIDC_CLIENT_ID:-}" && -n "${LITECR_OIDC_CLIENT_SECRET:-}" && -n "${LITECR_OIDC_REDIRECT_URI:-}" ]] || fail OIDC_CONFIG_REQUIRED
[[ "$LITECR_OIDC_REDIRECT_URI" == "${LITECR_WEB_ORIGIN%/}/api/v1/access/oidc/callback" ]] || fail OIDC_REDIRECT_MISMATCH
bind="${LITECR_BIND:-127.0.0.1:8080}"
if [[ "$bind" =~ ^(127\.[0-9]{1,3}\.[0-9]{1,3}\.[0-9]{1,3}):([0-9]{1,5})$ ]]; then
  bind_ip="${BASH_REMATCH[1]}"; bind_port="${BASH_REMATCH[2]}"
  IFS=. read -r _ octet2 octet3 octet4 <<< "$bind_ip"
  (( 10#$octet2 <= 255 && 10#$octet3 <= 255 && 10#$octet4 <= 255 && 10#$bind_port >= 1 && 10#$bind_port <= 65535 )) || fail API_BIND_NOT_PRIVATE
elif [[ "$bind" =~ ^\[::1\]:([0-9]{1,5})$ ]]; then
  bind_port="${BASH_REMATCH[1]}"; (( 10#$bind_port >= 1 && 10#$bind_port <= 65535 )) || fail API_BIND_NOT_PRIVATE
else
  fail API_BIND_NOT_PRIVATE
fi

unset LITECR_MIGRATION_DATABASE_URL
declare -A migration_allowed=([LITECR_MIGRATION_DATABASE_URL]=1 [LITECR_DATABASE_MAX_CONNECTIONS]=1)
declare -A migration_seen=()
while IFS= read -r line || [[ -n "$line" ]]; do
  [[ -z "$line" || "$line" == \#* ]] && continue
  [[ "$line" == *=* && "$line" != *$'\r'* ]] || fail MIGRATION_ENV_SYNTAX
  key="${line%%=*}"; value="${line#*=}"
  [[ -n "${migration_allowed[$key]+present}" && -z "${migration_seen[$key]+present}" ]] || fail MIGRATION_ENV_KEY
  migration_seen[$key]=1
  if [[ "$key" == LITECR_MIGRATION_DATABASE_URL ]]; then
    [[ "$value" == postgres://* || "$value" == postgresql://* ]] || fail MIGRATION_DATABASE_URL_INVALID
    [[ "$value" != "$double_quote"* && "$value" != "$single_quote"* ]] || fail MIGRATION_ENV_QUOTED_VALUE
  fi
done < "$migration_env"
[[ -n "${migration_seen[LITECR_MIGRATION_DATABASE_URL]+present}" ]] || fail MIGRATION_DATABASE_URL_REQUIRED

mapfile -t checksum_records < "$checksum"
(( ${#checksum_records[@]} == 1 )) || fail CHECKSUM_SIDECAR_INVALID
checksum_record="${checksum_records[0]}"
checksum_pattern='^([0-9a-f]{64})  (.+)$'
[[ "$checksum_record" =~ $checksum_pattern ]] || fail CHECKSUM_SIDECAR_INVALID
expected="${BASH_REMATCH[1]}"; filename="${BASH_REMATCH[2]}"
[[ "$filename" == "$(basename -- "$bundle")" ]] || fail CHECKSUM_SIDECAR_INVALID
[[ "$(sha256sum -- "$bundle" | awk '{print $1}')" == "$expected" ]] || fail BUNDLE_CHECKSUM_INVALID
size="$(stat -c '%s' -- "$bundle")"; need_kb=$(( size / 512 + 524288 )); free_kb="$(df -Pk /opt | awk 'NR==2 {print $4}')"
[[ "$free_kb" =~ ^[0-9]+$ ]] && (( free_kb >= need_kb )) || fail DISK_SPACE

pending_tmp=""; release_tmp=""
temp="$(mktemp -d /var/tmp/litecr-install.XXXXXXXX)"; trap 'rm -rf -- "$temp"; [[ -z "${pending_tmp:-}" ]] || rm -f -- "$pending_tmp"; [[ -z "${release_tmp:-}" ]] || rm -rf -- "$release_tmp"' EXIT
while IFS= read -r entry; do
  [[ "$entry" != /* && "$entry" != .. && "$entry" != ../* && "$entry" != */../* && "$entry" != */.. ]] || fail ARCHIVE_PATH
done < <(tar -tzf "$bundle")
while IFS= read -r listing; do
  kind="${listing:0:1}"
  [[ "$kind" == d || "$kind" == - ]] || fail ARCHIVE_LINK_OR_SPECIAL_FILE
done < <(tar -tvzf "$bundle")
tar -xzf "$bundle" -C "$temp" --no-same-owner
mapfile -t release_dirs < <(find "$temp" -mindepth 1 -maxdepth 1 -type d -name 'litecr-*-linux-x86_64' -print)
(( ${#release_dirs[@]} == 1 )) || fail ARCHIVE_LAYOUT
release="${release_dirs[0]}"
[[ -x "$release/bin/litecr" && -f "$release/SHA256SUMS" && -f "$release/share/metadata/version.json" \
  && -x "$release/share/tools/operations/grant-runtime-database-access.sh" \
  && -x "$release/share/tools/operations/activate-pending-release.sh" \
  && -x "$release/share/tools/operations/backup-postgres.sh" \
  && -x "$release/share/tools/operations/restore-postgres-isolated.sh" ]] || fail RELEASE_CONTENTS
(cd "$release" && sha256sum --check --status -- SHA256SUMS) || fail RELEASE_CHECKSUM
name="$(basename -- "$release")"; version="${name#litecr-}"; version="${version%-linux-x86_64}"
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9A-Za-z.+-]+$ ]] || fail RELEASE_VERSION
"$release/bin/litecr" version --json > "$temp/version.json" 2>/dev/null || fail VERSION_COMMAND
cmp -s -- "$temp/version.json" "$release/share/metadata/version.json" || fail VERSION_METADATA
version_json="$(<"$temp/version.json")"
version_prefix="{\"application_version\":\"$version\",\"schema_version\":"
[[ "$version_json" == "$version_prefix"* ]] || fail RELEASE_VERSION_MISMATCH
schema_version="${version_json#"$version_prefix"}"
[[ "$schema_version" =~ ^[0-9]+\}$ ]] || fail RELEASE_VERSION_MISMATCH
curl --proto '=https' --tlsv1.2 --fail --silent --show-error --max-time 10 --output /dev/null \
  "${LITECR_OIDC_ISSUER_URL%/}/.well-known/openid-configuration" 2>/dev/null || fail OIDC_UNREACHABLE
doctor_status=0; "$release/bin/litecr" doctor > "$temp/doctor.txt" 2>&1 || doctor_status=$?
if (( doctor_status != 0 )); then
  sed -E 's/\x1B\[[0-9;]*[[:alpha:]]//g' "$temp/doctor.txt" > "$temp/doctor-plain.txt"
  grep -Eq 'error=liteCR schema is not installed; run litecr migrate$' "$temp/doctor-plain.txt" \
    || grep -Eq 'error=database schema [0-9]+ is older than application schema [0-9]+; run litecr migrate$' "$temp/doctor-plain.txt" \
    || fail DATABASE_UNREACHABLE
fi
if [[ "${dry_run:-0}" == 1 ]]; then
  printf 'liteCR installer preflight passed; no system files or services were changed\n'
  exit 0
fi

installer_lock_dir=/run/lock/litecr-installer
if [[ -e "$installer_lock_dir" || -L "$installer_lock_dir" ]]; then
  [[ -d "$installer_lock_dir" && ! -L "$installer_lock_dir" ]] || fail INSTALLER_LOCK_PATH
  [[ "$(stat -c '%U:%a' -- "$installer_lock_dir")" == root:700 ]] || fail INSTALLER_LOCK_PATH
else
  mkdir -m 0700 -- "$installer_lock_dir" || fail INSTALLER_LOCK_PATH
fi
[[ ! -L "$installer_lock_dir/lock" ]] || fail INSTALLER_LOCK_PATH
exec 8<>"$installer_lock_dir/lock"
flock -n 8 || fail INSTALLER_ALREADY_RUNNING

root=/opt/litecr; target="$root/releases/$version"
verify_existing_release() {
  local candidate="$1"
  [[ -f "$candidate/SHA256SUMS" && ! -L "$candidate/SHA256SUMS" ]] || fail RELEASE_TARGET_CHECKSUM_INVALID
  cmp -s -- "$release/SHA256SUMS" "$candidate/SHA256SUMS" || fail RELEASE_ALREADY_DIFFERS
  (cd "$candidate" && sha256sum --check --status -- SHA256SUMS) || fail RELEASE_TARGET_CHECKSUM_INVALID
}

# The installer runs as root. Validate the complete destination ancestry before
# inspecting existing managed files or creating paths/copying secrets into it.
protected_destination_directory() {
  local path="$1" owner mode
  [[ -d "$path" && ! -L "$path" ]] || fail INSTALL_PATH_NOT_DIRECTORY
  owner="$(stat -c '%u' -- "$path")"; mode="$(stat -c '%a' -- "$path")"
  (( owner == 0 )) || fail INSTALL_PATH_OWNER
  (( (8#$mode & 022) == 0 )) || fail INSTALL_PATH_WRITABLE
}
for path in /opt /etc /etc/systemd /etc/systemd/system /var /var/lib; do
  protected_destination_directory "$path"
done
for path in /opt/litecr /opt/litecr/releases /etc/litecr; do
  if [[ -e "$path" || -L "$path" ]]; then protected_destination_directory "$path"; fi
done
protected_service_data_directory() {
  local path="$1" mode
  [[ -d "$path" && ! -L "$path" ]] || fail INSTALL_PATH_NOT_DIRECTORY
  mode="$(stat -c '%a' -- "$path")"
  (( (8#$mode & 022) == 0 )) || fail INSTALL_PATH_WRITABLE
}
for path in /var/lib/litecr /var/lib/litecr-migrate; do
  if [[ -e "$path" || -L "$path" ]]; then protected_service_data_directory "$path"; fi
done

if [[ -e "$target" || -L "$target" ]]; then
  [[ -d "$target" && ! -L "$target" ]] || fail RELEASE_TARGET_NOT_DIRECTORY
  protected_destination_directory "$target"
  verify_existing_release "$target"
fi
protected_existing_config() {
  local file="$1" expected_group="$2" owner_group_mode
  [[ -f "$file" && ! -L "$file" ]] || fail CONFIG_FILE_NOT_REGULAR
  owner_group_mode="$(stat -c '%U:%G:%a' -- "$file")"
  [[ "$owner_group_mode" == "root:${expected_group}:640" ]] || fail CONFIG_FILE_OWNERSHIP_OR_MODE
}
if [[ -e /etc/litecr/litecr.env || -L /etc/litecr/litecr.env ]]; then
  protected_existing_config /etc/litecr/litecr.env litecr
  cmp -s -- "$runtime_env" /etc/litecr/litecr.env || fail RUNTIME_CONFIG_EXISTS
fi
if [[ -e /etc/litecr/migration.env || -L /etc/litecr/migration.env ]]; then
  protected_existing_config /etc/litecr/migration.env litecr-migrate
  cmp -s -- "$migration_env" /etc/litecr/migration.env || fail MIGRATION_CONFIG_EXISTS
fi
[[ ! -e /opt/litecr/current || -L /opt/litecr/current ]] || fail CURRENT_IS_NOT_SYMLINK
[[ ! -e /opt/litecr/pending || -L /opt/litecr/pending ]] || fail PENDING_IS_NOT_SYMLINK
active_release=""
if [[ -L /opt/litecr/current ]]; then
  active_release="$(readlink -e -- /opt/litecr/current 2>/dev/null || true)"
  [[ "$active_release" == "$root"/releases/* && -d "$active_release" ]] || fail CURRENT_RELEASE_INVALID
fi
declare -A replace_unit=()
for unit in litecr-api.service litecr-migrate.service litecr-activate.service; do
  unit_file="/etc/systemd/system/$unit"; template="$release/share/templates/$unit"
  [[ -f "$template" ]] || fail SYSTEMD_TEMPLATE_MISSING
  if [[ -e "$unit_file" ]]; then
    if cmp -s -- "$template" "$unit_file"; then
      :
    elif [[ -n "$active_release" && -f "$active_release/share/templates/$unit" ]] \
      && cmp -s -- "$active_release/share/templates/$unit" "$unit_file"; then
      replace_unit[$unit]=1
    else
      fail SYSTEMD_UNIT_DIFFERS
    fi
  fi
done

if ! id litecr >/dev/null 2>&1; then
  getent group litecr >/dev/null 2>&1 || groupadd --system litecr
  useradd --system --gid litecr --home-dir /var/lib/litecr --shell /usr/sbin/nologin litecr
fi
if ! id litecr-migrate >/dev/null 2>&1; then
  getent group litecr-migrate >/dev/null 2>&1 || groupadd --system litecr-migrate
  useradd --system --gid litecr-migrate --home-dir /var/lib/litecr-migrate --shell /usr/sbin/nologin litecr-migrate
fi
install -d -o root -g root -m 0755 -- /etc/litecr /var/lib/litecr /var/lib/litecr-migrate
operation_lock_dir=/run/lock/litecr-release
if [[ -e "$operation_lock_dir" || -L "$operation_lock_dir" ]]; then
  [[ -d "$operation_lock_dir" && ! -L "$operation_lock_dir" ]] || fail RELEASE_LOCK_PATH
  [[ "$(stat -c '%U:%G:%a' -- "$operation_lock_dir")" == root:litecr-migrate:750 ]] || fail RELEASE_LOCK_PATH
else
  mkdir -m 0750 -- "$operation_lock_dir" || fail RELEASE_LOCK_PATH
  chown root:litecr-migrate "$operation_lock_dir"
fi
if [[ ! -e "$operation_lock_dir/lock" && ! -L "$operation_lock_dir/lock" ]]; then
  install -o root -g litecr-migrate -m 0660 /dev/null "$operation_lock_dir/lock"
fi
[[ -f "$operation_lock_dir/lock" && ! -L "$operation_lock_dir/lock" ]] || fail RELEASE_LOCK_NOT_REGULAR
[[ "$(stat -c '%U:%G:%a' -- "$operation_lock_dir/lock")" == root:litecr-migrate:660 ]] || fail RELEASE_LOCK_PATH
exec 9<>"$operation_lock_dir/lock"
flock -n 9 || fail RELEASE_OPERATION_IN_PROGRESS
chown litecr:litecr /var/lib/litecr; chown litecr-migrate:litecr-migrate /var/lib/litecr-migrate
if [[ -e "$target" ]]; then verify_existing_release "$target"
else
  install -d -o root -g root -m 0755 -- "$root/releases"
  release_tmp="$(mktemp -d "$root/releases/.${version}.XXXXXXXX")"
  cp -a -- "$release/." "$release_tmp/"
  chown -R root:root -- "$release_tmp"; chmod -R go-w -- "$release_tmp"; chmod 0755 -- "$release_tmp"
  verify_existing_release "$release_tmp"
  [[ ! -e "$target" && ! -L "$target" ]] || fail RELEASE_TARGET_EXISTS
  mv -T -- "$release_tmp" "$target"
  release_tmp=""
fi
if [[ ! -e /etc/litecr/litecr.env ]]; then install -o root -g litecr -m 0640 -- "$runtime_env" /etc/litecr/litecr.env; fi
if [[ ! -e /etc/litecr/migration.env ]]; then install -o root -g litecr-migrate -m 0640 -- "$migration_env" /etc/litecr/migration.env; fi
pending="$(readlink -- /opt/litecr/pending 2>/dev/null || true)"
if [[ "$pending" != "$target" ]]; then
  pending_tmp="/opt/litecr/.pending.$$.new"
  ln -s -- "$target" "$pending_tmp"; mv -Tf -- "$pending_tmp" /opt/litecr/pending
  pending_tmp=""
fi
for unit in litecr-api.service litecr-migrate.service litecr-activate.service; do
  unit_file="/etc/systemd/system/$unit"; template="$target/share/templates/$unit"
  if [[ ! -e "$unit_file" || -n "${replace_unit[$unit]+present}" ]]; then
    temporary="/etc/systemd/system/.${unit}.$$.new"
    install -o root -g root -m 0644 -- "$template" "$temporary"
    mv -Tf -- "$temporary" "$unit_file"
  fi
done
systemctl daemon-reload
printf 'liteCR %s staged at %s. Migrations were not run.\n' "$version" "$target"
printf 'The active release was not changed. Migrations use /opt/litecr/pending.\n'
printf 'After confirming a current backup, run: systemctl start litecr-migrate.service\n'
printf 'After migration succeeds, run: systemctl start litecr-activate.service\n'
printf 'Then activate/restart the API with: systemctl enable --now litecr-api.service (first install) or systemctl restart litecr-api.service (upgrade)\n'
