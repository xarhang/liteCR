#!/usr/bin/env bash
set -euo pipefail
set +x

if [[ $# -ne 2 || ! $1 =~ ^[a-f0-9]{16}$ || $2 != "litecr_admin_e2e_$1" ]]; then
  echo "usage: $0 <16-hex-run-id> <exact-litecr_admin_e2e-run-database>" >&2
  exit 2
fi
secret_file="$HOME/.litecr-dev/admin-e2e.env"
if [[ ! -f $secret_file || $(stat -c '%a' "$secret_file") != 600 ]]; then
  echo "protected ~/.litecr-dev/admin-e2e.env (mode 0600) is required" >&2
  exit 2
fi
set -a
. "$secret_file"
set +a

database_present=$(PGDATABASE=postgres psql -Atq -v ON_ERROR_STOP=1 -c "SELECT 1 FROM pg_database WHERE datname = '$2'") || {
  echo 'could not inspect the PostgreSQL database catalog' >&2
  exit 1
}
if [[ $database_present != 1 ]]; then
  echo "expected disposable run database is absent: $2" >&2
  exit 1
fi
printf 'verified disposable database %s\n' "$2"
