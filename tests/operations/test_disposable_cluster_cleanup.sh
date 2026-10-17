#!/usr/bin/env bash
set -euo pipefail
set +x
umask 077

repo="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd -P)"
source "$repo/tools/operations/cleanup-owned-postgres-cluster.sh"
scratch="$(mktemp -d /tmp/litecr-p6-recovery-schema16.XXXXXXXX)"
chmod 700 "$scratch"
scratch="$(realpath -e -- "$scratch")"
printf 'litecr-p6-recovery-schema16-v1\n' > "$scratch/OWNER"
chmod 600 "$scratch/OWNER"
fake_pg_ctl="$scratch/fake-pg-ctl"
cat > "$fake_pg_ctl" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
data=""
command_name=""
while (($#)); do
  case "$1" in
    -D) data="$2"; shift 2 ;;
    stop|status) command_name="$1"; shift ;;
    *) shift ;;
  esac
done
case "$command_name" in
  stop)
    if [[ "${CLEANUP_TEST_MODE:-}" == stop-fails ]]; then
      echo 'synthetic stop failure' >&2
      exit 8
    fi
    rm -f -- "$data/postmaster.pid"
    exit 0
    ;;
  status)
    if [[ -e "$data/postmaster.pid" ]]; then
      echo 'synthetic server still running' >&2
      exit 0
    fi
    echo 'synthetic server stopped' >&2
    exit 3
    ;;
  *) exit 64 ;;
esac
MOCK
chmod 700 "$fake_pg_ctl"

failed_root="$scratch/failure-case"
mkdir -m 700 "$failed_root"
printf 'litecr-p6-recovery-schema16-v1\n' > "$failed_root/OWNER"
chmod 600 "$failed_root/OWNER"
mkdir -m 700 "$failed_root/postgres-data"
printf '12345\n%s\n' "$failed_root/postgres-data" > "$failed_root/postgres-data/postmaster.pid"
set +e
failure_output="$(CLEANUP_TEST_MODE=stop-fails litecr_cleanup_owned_postgres_cluster \
  "$failed_root" "$failed_root/postgres-data" "$fake_pg_ctl" 2>&1)"
failure_rc=$?
set -e
[[ "$failure_rc" == 1 ]] || { echo 'expected failed cleanup status' >&2; exit 1; }
[[ -d "$failed_root/postgres-data" && -f "$failed_root/postgres-data/postmaster.pid" ]] \
  || { echo 'failed cleanup removed cluster data' >&2; exit 1; }
[[ "$failure_output" == *"RECOVERY_CLEANUP_PRESERVED_PATH: reason=PG_CTL_STOP_FAILED_RC_8_AND_CLUSTER_NOT_CONFIRMED_STOPPED root=$failed_root data=$failed_root/postgres-data"* ]] \
  || { echo 'failed cleanup did not report exact preserved path/error' >&2; printf '%s\n' "$failure_output" >&2; exit 1; }

success_root="$scratch/success-case"
mkdir -m 700 "$success_root"
printf 'litecr-p6-recovery-schema16-v1\n' > "$success_root/OWNER"
chmod 600 "$success_root/OWNER"
mkdir -m 700 "$success_root/postgres-data"
printf '12346\n%s\n' "$success_root/postgres-data" > "$success_root/postgres-data/postmaster.pid"
litecr_cleanup_owned_postgres_cluster "$success_root" "$success_root/postgres-data" "$fake_pg_ctl"
[[ ! -e "$success_root" ]] || { echo 'successful cleanup retained owned stopped cluster' >&2; exit 1; }

schema23_root="$(mktemp -d /tmp/litecr-p6-recovery-schema23.XXXXXXXX)"
chmod 700 "$schema23_root"
schema23_root="$(realpath -e -- "$schema23_root")"
printf 'litecr-p6-recovery-schema23-v1\n' > "$schema23_root/OWNER"
chmod 600 "$schema23_root/OWNER"
mkdir -m 700 "$schema23_root/postgres-data"
printf '12347\n%s\n' "$schema23_root/postgres-data" > "$schema23_root/postgres-data/postmaster.pid"
litecr_cleanup_owned_postgres_cluster "$schema23_root" "$schema23_root/postgres-data" "$fake_pg_ctl" 23
[[ ! -e "$schema23_root" ]] || { echo 'successful schema-23 cleanup retained owned stopped cluster' >&2; exit 1; }

schema25_root="$(mktemp -d /tmp/litecr-p6-recovery-schema25.XXXXXXXX)"
chmod 700 "$schema25_root"
schema25_root="$(realpath -e -- "$schema25_root")"
printf 'litecr-p6-recovery-schema25-v1\n' > "$schema25_root/OWNER"
chmod 600 "$schema25_root/OWNER"
mkdir -m 700 "$schema25_root/postgres-data"
printf '12348\n%s\n' "$schema25_root/postgres-data" > "$schema25_root/postgres-data/postmaster.pid"
litecr_cleanup_owned_postgres_cluster "$schema25_root" "$schema25_root/postgres-data" "$fake_pg_ctl" 25
[[ ! -e "$schema25_root" ]] || { echo 'successful schema-25 cleanup retained owned stopped cluster' >&2; exit 1; }

for cleanup_schema in 26 27; do
  current_root="$(mktemp -d "/tmp/litecr-p6-recovery-schema${cleanup_schema}.XXXXXXXX")"
  chmod 700 "$current_root"
  current_root="$(realpath -e -- "$current_root")"
  printf 'litecr-p6-recovery-schema%s-v1\n' "$cleanup_schema" > "$current_root/OWNER"
  chmod 600 "$current_root/OWNER"
  mkdir -m 700 "$current_root/postgres-data"
  printf '12349\n%s\n' "$current_root/postgres-data" > "$current_root/postgres-data/postmaster.pid"
  litecr_cleanup_owned_postgres_cluster "$current_root" "$current_root/postgres-data" "$fake_pg_ctl" "$cleanup_schema"
  [[ ! -e "$current_root" ]] || { echo "successful schema-$cleanup_schema cleanup retained owned stopped cluster" >&2; exit 1; }
done

# This test owns the scratch root, including the intentionally preserved failure fixture.
rm -rf -- "$scratch"
echo 'Disposable PostgreSQL cleanup failure/success cases passed.'
