# PostgreSQL runtime and migration roles

Use separate runtime and migration database identities. The role names below are the baseline enforced by the bundled grant scripts; the application does not create database roles or manage passwords. Rehearse the exact release and ACL policy on a disposable database before operational use.

## Boundary and assumptions

- Use a dedicated liteCR database. `litecr_migrator` is its non-superuser owner and is the only login used by `litecr migrate`. This is required because migration 0001 creates `pg_trgm` and schema `litecr`, while later migrations create schema `matching` and the SQLx ledger. If the database owner or schema ownership must differ, stop and obtain the database owner's reviewed ACL model; the provisioning script intentionally refuses to guess or take ownership.
- `litecr_runtime` is a separate non-superuser login used by `litecr serve` and `litecr doctor`. It has no role memberships, database/schema DDL rights, SQLx migration-ledger write rights, or `consumer_job` UPDATE rights. It receives application data privileges on the `litecr`/`matching` schemas; the current single-process application uses one runtime DB identity for its domain modules.
- `litecr_worker` is an optional separate non-superuser login used only by `litecr worker`. Its ACL is limited to queue reads/updates, receipt/evidence writes, and execution of the fixed source-snapshot lock function; it cannot read person, review, localization or country-profile data and cannot update registry source facts. Keep its root-owned environment file readable only by the `litecr-worker` OS account. Do not provision or enable this role/unit on an operational host until the queue recovery, release and readiness gates in [the Beta limits](../BETA.md) are accepted.
- Current app behavior requires `SELECT`, `INSERT`, and `UPDATE` on mutable `litecr` tables, `SELECT`/`INSERT` on matching evidence, and `DELETE` only on `litecr.person_identifier` for guarded migration reversal. Audit/outbox tables and matching evidence are append-only from this role. Table-level app privileges do not replace API authentication, tenant authorization, audit controls, OS/network isolation, or a future split between API and migration operations.
- The ACL procedure gives `litecr_runtime` `SELECT` on `public._sqlx_migrations` so read-only schema checks can work, but does not grant INSERT, UPDATE, DELETE, or TRUNCATE. The migration account remains the owner of the database, app schemas, objects, and ledger.
- The procedure does not create or modify roles, set passwords, change database ownership, revoke privileges from `PUBLIC`, or alter unrelated databases. It fails closed if the named role attributes, memberships, or owners differ from this baseline.

## DBA bootstrap

Have the database owner create these names only after checking that they do not already exist. Omit the worker role and its CONNECT grant when the worker is not being evaluated. Use the organization's approved secret manager, peer/certificate authentication, or an interactive `psql` `\\password` workflow to set credentials. Do not put passwords in SQL files, shell arguments, command history, source control, or this runbook.

```sql
CREATE ROLE litecr_migrator LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS;
CREATE ROLE litecr_runtime LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS;
CREATE ROLE litecr_worker LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS;
CREATE DATABASE litecr OWNER litecr_migrator;
GRANT CONNECT ON DATABASE litecr TO litecr_migrator, litecr_runtime, litecr_worker;
```

Run those statements as a DBA on the dedicated target. Do not run them against a shared/dev database or reuse an existing account. The DBA must separately decide whether `PUBLIC` should retain `CONNECT` on this database and account for approved backup/restore identities; this procedure does not revoke other access. Store only the resulting connection settings in the protected service environments: API `/etc/litecr/litecr.env` carries `LITECR_DATABASE_URL` for `litecr_runtime`; migration `/etc/litecr/migration.env` carries `LITECR_MIGRATION_DATABASE_URL` for `litecr_migrator`. Keep the migration file unreadable by the API service account as specified in [the Beta limits](../BETA.md).

## Install and refresh grants

After staging the release and provisioning the empty database, use the pending release for schema changes. The migration service serializes access with release activation:

```sh
sudo systemctl start litecr-migrate.service
sudo -i
while IFS= read -r line; do
  case "$line" in ''|\#*) continue ;; esac
  case "$line" in
    LITECR_MIGRATION_DATABASE_URL=*) export LITECR_MIGRATION_DATABASE_URL="${line#*=}" ;;
    LITECR_DATABASE_MAX_CONNECTIONS=*) ;;
    *) echo 'Unexpected migration environment key'; exit 1 ;;
  esac
done < /etc/litecr/migration.env
bash /opt/litecr/pending/share/tools/operations/grant-runtime-database-access.sh
unset LITECR_MIGRATION_DATABASE_URL
exit
sudo systemctl start litecr-activate.service
```

The snippet reads only permitted environment entries as literal data; it does not execute the file or print credentials. The activation unit runs the pending binary's `doctor` check before changing `current`. Do not start/restart the API until activation succeeds.

The ACL script expects `current_user = litecr_migrator`, that role to own the dedicated database and both app schemas, and `litecr_runtime` to be a standalone low-privilege login. It validates the SQLx ledger owner and rejects pre-existing runtime DDL/ledger-write access before applying grants. It is safe to re-run after a reviewed migration; new tables receive read/insert defaults, while the explicit refresh reapplies current update and reversal-delete grants. Review new migration tables before granting runtime access. The script uses a private temporary libpq service file and passfile if the protected migration URL contains a password; it never prints or writes the URL into the repository.

The API must use only `LITECR_DATABASE_URL`. Run `litecr doctor` from the API environment to verify its restricted account can connect and read schema metadata. Keep `litecr migrate` as an explicit, separately credentialed operator action.

### Local WAL commit policy

The shared Rust PostgreSQL pool sets `synchronous_commit=on` for every physical API/worker connection. Startup refuses to connect if PostgreSQL reports `fsync=off`, and `/health/ready` repeats the durability preflight. Do not disable `fsync` for liteCR. If readiness fails, have the database operator inspect effective PostgreSQL settings and service configuration before enabling traffic.

This means successful commits wait for the primary's local WAL flush, subject to the storage stack honoring flush requests. It does not require or prove a synchronous standby, safe promotion/fencing, an off-host backup, or RPO=0. Those remain separate owner-approved deployment and recovery gates.

### Optional advisory worker role

The worker role is a separate interface and is not needed to run the API. The DBA creates the `litecr_worker` login above and stores its URL only in `/etc/litecr/worker.env`, owned by `root:litecr-worker` with mode `0640`. The API account and migration service account must not be able to read that file. Do not copy either URL into another service file.

After applying all embedded migrations with the release-matched binary, run the worker ACL procedure as the database owner/migrator from the protected migration environment:

```sh
sudo -i
while IFS= read -r line; do
  case "$line" in ''|\#*) continue ;; esac
  case "$line" in
    LITECR_MIGRATION_DATABASE_URL=*) export LITECR_MIGRATION_DATABASE_URL="${line#*=}" ;;
    LITECR_DATABASE_MAX_CONNECTIONS=*) ;;
    *) echo 'Unexpected migration environment key'; exit 1 ;;
  esac
done < /etc/litecr/migration.env
bash /opt/litecr/pending/share/tools/operations/grant-worker-database-access.sh
unset LITECR_MIGRATION_DATABASE_URL
exit
```

The grant wrapper requires `psql` and Python 3 and keeps the connection password in a private temporary passfile rather than argv or repository files.

The grant script checks the migrator's database/schema ownership, worker role attributes and memberships, and supported schema before applying narrow queue/evidence privileges. It checks that the source-lock function has no PUBLIC execute grant. The worker cannot update registry source facts or server-verified migration package data. The script does not create roles, set passwords, change ownership, or touch unrelated databases. Use the grant script shipped with the exact release; review and recheck its privileges after schema changes.

The worker process additionally checks `SELECT current_user = 'litecr_worker'` and exact schema compatibility at startup. It reads only `LITECR_WORKER_DATABASE_URL`, never falls back to either other service URL, and is launched only by the optional `litecr-worker` systemd template. The installer deliberately does not create `/etc/litecr/worker.enable` or enable the unit. The unit and grants are source/package artifacts only until clean-host installation, process termination/restart, lease recovery, load and readiness checks pass.

Before enabling the optional worker, verify its permissions, service identity, shutdown, lease expiry/reclaim, restart behavior, and impact on API load in an isolated synthetic environment. Do not reuse historical harnesses that require an older schema with a current binary.
