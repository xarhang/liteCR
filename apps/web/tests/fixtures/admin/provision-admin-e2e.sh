#!/usr/bin/env bash
set -euo pipefail
set +x

if [[ ( $# -ne 1 && $# -ne 2 ) || ! $1 =~ ^[a-f0-9]{16}$ ]]; then
  echo "usage: $0 <16-hex-run-id> [litecr-api-binary]" >&2
  exit 2
fi
run_id=$1
api_binary="${2:-}"
if [[ -n $api_binary && ( $api_binary != /* || ! -x $api_binary || -L $api_binary || $(basename -- "$api_binary") != litecr ) ]]; then
  echo "optional API binary must be an absolute, executable, non-symlink litecr file" >&2
  exit 2
fi
root=/mnt/d/liteCR
export PATH="$HOME/.cargo/bin:$PATH"
fixture_root="$root/infra/dev/keycloak"
archive="$fixture_root/.runtime/keycloak-26.8.0.zip"
expected_sha=7ed1de3fda2598369262613bf682aab7e233d80a38c405e91588f7a7454370a1
pg_bin="$(pg_config --bindir)"
state_root="$HOME/.litecr-dev/admin-e2e"
run_dir="$state_root/$run_id"

if [[ ! -x $pg_bin/initdb || ! -x $pg_bin/pg_ctl ]] \
  || ! pg_config --version | grep -Eq '^PostgreSQL (16\.15|17\.6|18\.6)([[:space:]]|$)'; then
  echo "PostgreSQL 16.15, 17.6, or 18.6 initdb and pg_ctl are required; no shared database will be touched" >&2
  exit 2
fi
if [[ ! -f $archive || $(sha256sum "$archive" | awk '{print $1}') != "$expected_sha" ]]; then
  echo "the verified Keycloak 26.8.0 archive is missing or its checksum differs" >&2
  exit 2
fi
if [[ -L $state_root || -L $run_dir || -e $run_dir ]]; then
  echo "refusing existing or symlinked fixture path: $run_dir" >&2
  exit 2
fi

install -d -m 700 "$HOME/.litecr-dev" "$state_root"
chmod 700 "$HOME/.litecr-dev" "$state_root"
umask 077
mkdir -m 700 "$run_dir"
printf 'liteCR-admin-e2e-v1 %s\n' "$run_id" > "$run_dir/OWNER"
chmod 600 "$run_dir/OWNER"
setup_complete=0
cleanup_failed_setup() {
  status=$?
  if [[ $status -ne 0 && $setup_complete -eq 0 ]]; then
    bash "$root/apps/web/tests/fixtures/admin/cleanup-admin-e2e.sh" "$run_id" >/dev/null 2>&1 || true
  fi
  exit "$status"
}
trap cleanup_failed_setup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

translator_subject=$(uuidgen | tr '[:upper:]' '[:lower:]')
reviewer_subject=$(uuidgen | tr '[:upper:]' '[:lower:]')
publisher_subject=$(uuidgen | tr '[:upper:]' '[:lower:]')
administrator_subject=$(uuidgen | tr '[:upper:]' '[:lower:]')
registrar_subject=$(uuidgen | tr '[:upper:]' '[:lower:]')
port_hex=${run_id:0:4}
pg_port=$((20000 + 16#$port_hex % 30000))
database="litecr_admin_e2e_${run_id}"
database_password=$(openssl rand -hex 24)

mkdir -m 700 "$run_dir/socket"
printf '%s\n' "$database_password" > "$run_dir/pg-password"
chmod 600 "$run_dir/pg-password"
"$pg_bin/initdb" -D "$run_dir/postgres" --username=xarhang --pwfile="$run_dir/pg-password" \
  --auth-local=peer --auth-host=scram-sha-256 --encoding=UTF8 --locale=C.UTF-8 >/dev/null
"$pg_bin/pg_ctl" -D "$run_dir/postgres" -l "$run_dir/postgres.log" \
  -o "-h 127.0.0.1 -p $pg_port -k $run_dir/socket" -w start >/dev/null

PGHOST=127.0.0.1 PGPORT="$pg_port" PGUSER=xarhang PGPASSWORD="$database_password" \
  createdb --template=template0 "$database"
database_url="postgres://xarhang:${database_password}@127.0.0.1:${pg_port}/${database}"
printf 'export LITECR_E2E_ADMIN_RUN_ID=%q\n' "$run_id" > "$run_dir/runtime.env"
if [[ -n $api_binary ]]; then
  printf 'export LITECR_E2E_BINARY=%q\n' "$api_binary" >> "$run_dir/runtime.env"
fi
printf 'export LITECR_E2E_ADMIN_DATABASE_NAME=%q\n' "$database" >> "$run_dir/runtime.env"
printf 'export LITECR_DATABASE_URL=%q\n' "$database_url" >> "$run_dir/runtime.env"
printf 'export LITECR_MIGRATION_DATABASE_URL=%q\n' "$database_url" >> "$run_dir/runtime.env"
printf 'export LITECR_ENV=development\n' >> "$run_dir/runtime.env"
printf 'export LITECR_BIND=127.0.0.1:18080\n' >> "$run_dir/runtime.env"
printf 'export LITECR_OIDC_ISSUER_URL=http://127.0.0.1:18081/realms/litecr\n' >> "$run_dir/runtime.env"
printf 'export LITECR_OIDC_CLIENT_ID=litecr-web\n' >> "$run_dir/runtime.env"
printf 'export LITECR_OIDC_REDIRECT_URI=http://127.0.0.1:4173/api/v1/access/oidc/callback\n' >> "$run_dir/runtime.env"
printf 'export LITECR_WEB_ORIGIN=http://127.0.0.1:4173\n' >> "$run_dir/runtime.env"
printf 'export LITECR_COOKIE_SECURE=false\n' >> "$run_dir/runtime.env"
chmod 600 "$run_dir/runtime.env"

mkdir -m 700 "$run_dir/keycloak-install"
unzip -q "$archive" -d "$run_dir/keycloak-install"
python3 "$root/apps/web/tests/fixtures/admin/prepare-realm.py" \
  "$run_dir" "$fixture_root/realm/litecr-realm.json" \
  "$translator_subject" "$reviewer_subject" "$publisher_subject" "$administrator_subject" \
  "$registrar_subject"
source "$run_dir/runtime-secrets.env"
printf 'export LITECR_OIDC_CLIENT_SECRET=%q\n' "$LITECR_E2E_ADMIN_OIDC_CLIENT_SECRET" >> "$run_dir/runtime.env"
chmod 600 "$run_dir/runtime.env"

# Run the product's embedded migrations against this exact private database.
source "$run_dir/runtime.env"
cd "$root"
if [[ -n $api_binary ]]; then
  "$api_binary" migrate
else
  cargo run --locked -p litecr-server -- migrate
fi

PGHOST=127.0.0.1 PGPORT="$pg_port" PGUSER=xarhang PGPASSWORD="$database_password" \
  psql -q -v ON_ERROR_STOP=1 -d "$database" \
    -v run_id="$run_id" \
    -v translator_subject="$translator_subject" \
    -v reviewer_subject="$reviewer_subject" \
    -v publisher_subject="$publisher_subject" \
    -v administrator_subject="$administrator_subject" \
    -v registrar_subject="$registrar_subject" \
    -f "$root/apps/web/tests/fixtures/admin/provision-grants.sql"

bash "$root/apps/web/tests/fixtures/admin/start-admin-e2e-services.sh" "$run_id"

setup_complete=1
trap - EXIT
printf 'prepared isolated admin E2E fixture for run %s; session files belong under %s\n' "$run_id" "$run_dir"
