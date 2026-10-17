# PostgreSQL backup and isolated restore

Use these procedures with the matching release on an isolated rehearsal target before relying on them. Tests use synthetic data; independent host recovery, IdP/key recovery, off-host transfer, retention, and production RPO/RTO remain unvalidated.

## Scope and required inputs

The scripts in `tools/operations` create PostgreSQL custom-format logical dumps using `pg_dump` and a single MVCC snapshot. They do not copy a live `PGDATA` directory. The backup includes the application database contents and a sidecar manifest/checksum. It does not include IdP state, host configuration, secret values, certificates, release bundles, or external integration configuration; those have independent recovery procedures below.

The Windows PowerShell backup/restore scripts are experimental prototypes and are not included in the native Linux release. They are not an operational recovery path.

The server interface is: `litecr version --json` prints `application_version` and `schema_version` without connecting to the database, `litecr doctor` uses fixed redacted errors, and the API runtime environment file is `/etc/litecr/litecr.env`. The one-shot migration unit uses a separate `/etc/litecr/migration.env` with `LITECR_MIGRATION_DATABASE_URL`; migrations do not run at API startup. The backup scripts have their own, distinct interface: they accept a root-owned `LITECR_CONFIG_FILE`, which must not be confused with either server environment file. Do not source backup config as shell code. Its settings are:

## Data-loss prevention design (proposed; owner decision required)

The requested direction is to prevent loss of committed registry data. A candidate objective is **RPO 0 for acknowledged database transactions**, but this is not yet an approved or proven production guarantee. It protects only against a defined failure set; no design can promise zero loss after every possible simultaneous replica, site, credential, or media failure.

To meet that candidate for a primary-site failure, the production database design would need a synchronous standby in an independent failure domain and strict synchronous commit behavior so the client is not told a transaction committed until its WAL is durably flushed on the failover standby. Failover must fence the former primary and promote only a standby known to contain all acknowledged commits. Strict synchronous behavior can stop or block writes when the required standby is unavailable; that availability/latency tradeoff requires owner approval. Do not silently fall back to asynchronous acknowledgement while continuing to claim RPO 0.

Continue encrypted off-host base backups and continuous WAL archiving for point-in-time recovery and protection from operator error. Logical `pg_dump` snapshots in the current scripts are useful restore artifacts but do not provide continuous WAL replay. WAL archiving can lag until a segment is completed, and archive/storage or database configuration files need separate monitoring and recovery. Keep IdP state, keys, host configuration and release artifacts in their separately protected recovery procedures. See the PostgreSQL 18 guidance on [continuous archiving/PITR](https://www.postgresql.org/docs/18/continuous-archiving.html) and [synchronous and warm-standby replication](https://www.postgresql.org/docs/18/warm-standby.html).

Before any production claim, the DB/operations owner must set the measurable **RTO** and ratify the RPO/failure assumptions. A representative drill should issue uniquely traceable acknowledged writes with audit/outbox effects, fail or isolate the primary at controlled points, prove every acknowledged write survives promotion, prove unacknowledged requests are safely retryable/idempotent, test fencing and archive alarms, restore a backup plus WAL on an isolated host, reconcile identities/history/audit/outbox, and measure elapsed recovery time. Record what data loss remains possible under total correlated site failure. No such production-topology drill has been performed.

```text
LITECR_PG_DATABASE=litecr
LITECR_PG_USER=litecr_backup
LITECR_BACKUP_DIR=/var/lib/litecr-backups
LITECR_RELEASE_ID=<operator-supplied release identifier>
LITECR_PGHOST=/var/run/postgresql
LITECR_PGPORT=5432
LITECR_MIN_FREE_KB=1048576
```

Only the first four settings are required. The directory must be private, and the config and optional `PGPASSFILE` must be owned by root or the invoking service account and have no group/other permission bits. Use a least-privilege PostgreSQL role with read access sufficient for `pg_dump`, access to `litecr.schema_meta`, and no application write privileges. Keep passwords out of command arguments and this config file. `LITECR_RELEASE_ID` remains operator-supplied because the stable version JSON contains application and schema versions, not an immutable release identifier; do not parse provisional CLI output.

The backup records the actual server and client versions. Restore requires the same PostgreSQL major version; test cross-major upgrades separately before relying on them.

## Local backup

Install matching PostgreSQL client utilities from the supported operating-system packages. Create the config and backup directory with restrictive ownership/modes, provide `PGPASSFILE` only through the service environment, and run:

```sh
LITECR_CONFIG_FILE=/etc/litecr/backup.conf \
PGPASSFILE=/etc/litecr/backup.pgpass \
bash /opt/litecr/current/share/tools/operations/backup-postgres.sh --archive-only
```

The script refuses low free space, concurrent runs, malformed/duplicate/unknown config keys, and unsafe secret-file modes. On success, retain the `.dump`, `.manifest`, `.sha256`, and `.success` files as one set. The mode-0600 `.success` sidecar is atomically renamed into place only after the archive, manifest, and checksum have been published; it records the backup ID, dump filename, digest, and UTC completion time. A monitor can treat the newest `.success` timestamp as evidence that a local archive set completed, then independently check the referenced files and digest. A dump without its `.success` sidecar is not a completed backup set. Restore now validates this protected marker against the manifest and digest before any database command. This marker does not prove that an encrypted off-host transfer completed, that the files remain readable, or that a restore succeeds; monitor Restic/repository freshness and run restore drills separately. Protect local files and directory; they contain health registry data. Retention and alert thresholds must be set by the service owner; the script never prunes.

## Encrypted off-host transfer

The `--transfer` mode uses Restic’s encrypted repository format. It transfers the dump, manifest, checksum, and `.success` completion marker together. It does not provision or initialize a repository and it does not configure any destination. An authorized operator must provision an approved repository, network policy, retention policy, and separately recoverable repository password/key. Store the Restic password in a protected file, test that the recovery key can be retrieved by the approved custodians, and then configure `RESTIC_REPOSITORY` and `RESTIC_PASSWORD_FILE` in the backup service environment:

```sh
LITECR_CONFIG_FILE=/etc/litecr/backup.conf \
PGPASSFILE=/etc/litecr/backup.pgpass \
RESTIC_REPOSITORY=<approved-restic-repository> \
RESTIC_PASSWORD_FILE=/etc/litecr/restic-password \
bash /opt/litecr/current/share/tools/operations/backup-postgres.sh --transfer
```

Run `restic check` under the repository’s approved read-only/maintenance process and test retrieval to an isolated host. Never put repository credentials in shell history, config, logs, source control, or a command-line argument. Do not run `restic forget --prune` until the owner has approved retention and verified that protected snapshots remain restorable. Off-host transfer and isolated-host retrieval must be verified for the selected repository.

## Isolated restore rehearsal

Restore only onto a separately provisioned, disposable PostgreSQL cluster with a verified independent data directory, administrator boundary, and no network route/credentials to production. The marker binds the rehearsal to one PostgreSQL cluster system identifier; it is still an operator self-attestation, not proof that the cluster is isolated from production. Before using `litecr_restore_admin` below, apply the direct function `EXECUTE` grant and temporary `CREATEDB` setup documented in this section. After connecting to the intended disposable target, record its identifier in the protected marker:

```sh
umask 077
system_identifier="$(PGHOST=127.0.0.1 PGPORT=5432 PGUSER=litecr_restore_admin \
  psql -X -A -t --no-password --dbname=postgres \
  -c 'SELECT system_identifier FROM pg_control_system()')"
[[ "$system_identifier" =~ ^[0-9]{1,20}$ ]]
printf 'format=litecr-isolated-postgres-target-v2\nsystem_identifier=%s\n' \
  "$system_identifier" > /root/litecr-restore-isolated.marker
chmod 600 /root/litecr-restore-isolated.marker
```

Use the `restore-postgres-isolated.sh` tool from the matching installed release (`/opt/litecr/current/share/tools/operations/restore-postgres-isolated.sh`) and a root-owned config containing only the restore settings, for example:

```text
LITECR_PG_USER=litecr_restore_admin
LITECR_PGHOST=/var/run/postgresql
LITECR_PGPORT=5432
LITECR_RESTORE_DB_OWNER=litecr_runtime
```

Use a dedicated, non-superuser restore account. For a one-time rehearsal, grant only database creation and direct execute on the control-identity function, then revoke both after restore:

```sql
GRANT EXECUTE ON FUNCTION pg_catalog.pg_control_system() TO litecr_restore_admin;
ALTER ROLE litecr_restore_admin CREATEDB;
-- After the restore completes:
ALTER ROLE litecr_restore_admin NOCREATEDB;
REVOKE EXECUTE ON FUNCTION pg_catalog.pg_control_system() FROM litecr_restore_admin;
```

PostgreSQL documents `pg_control_system()` as returning control-file metadata including the system identifier ([control-data functions](https://www.postgresql.org/docs/18/functions-info.html#FUNCTIONS-INFO-CONTROL)). The disposable recovery harness revokes `PUBLIC` execute before testing this account, verifies the query is denied, grants only this function directly, and confirms the same non-superuser can query it and restore. The restore path does not require `pg_monitor` or superuser. On a cluster where `PUBLIC` already has function execute, inspect the function ACL and avoid broadening it further.

Set `PGPASSFILE` in the protected service environment. The script rejects inherited libpq connection, session, service, locale and internal-behavior environment settings except `PGPASSFILE`; configure the endpoint only through `LITECR_PGHOST` and `LITECR_PGPORT`, and use only a local socket or loopback host. This prevents ambient connection options, credentials, service files, TLS/session settings or config-directory overrides from silently changing restore behavior. Review the denylist against the supported PostgreSQL client version when updating it; see [PostgreSQL 18 libpq environment variables](https://www.postgresql.org/docs/18/libpq-envars.html). Use an unused database name matching `litecr_restore_<safe-name>`. The script requires the complete protected `.dump`, `.manifest`, `.sha256`, `.success` set and validates the completion marker’s format, fields, backup ID, dump filename, digest and timestamp against the manifest/checksum before any PostgreSQL command. It also compares the live target's `pg_catalog.pg_control_system().system_identifier` with the protected v2 isolation marker before creating the database, so an endpoint pointed at another PostgreSQL cluster fails closed. It then verifies the archive listing, target PostgreSQL major, and absence of the target database. It creates a new database and restores in one transaction. It does not drop or replace a database; a failed restore may leave the newly created database for inspection.

```sh
LITECR_CONFIG_FILE=/etc/litecr/restore-isolated.conf \
PGPASSFILE=/etc/litecr/restore.pgpass \
bash /opt/litecr/current/share/tools/operations/restore-postgres-isolated.sh \
  --dump /srv/restore-input/litecr-synthetic.dump \
  --target-db litecr_restore_drill \
  --isolation-marker /root/litecr-restore-isolated.marker
```

After restore, use the matching release’s `litecr version --json` to record application/schema versions, then run `litecr doctor` and read-only queries to verify schema readiness, expected synthetic fixture counts, aliases/public IDs, source assertions and versions, audit/outbox history, and any job state. `doctor` reports fixed redacted errors, including `LITECR_DATABASE_URL is required`, `database connection failed; verify service and credentials`, `could not read liteCR schema metadata; run liteCR migrate`, and `liteCR schema is not installed; run liteCR migrate`; incompatible-schema errors identify only the schema version values. Do not print names, identifiers, tokens, connection URLs, or row payloads into ordinary logs. Preserve the original backup and all diagnostic evidence under the organization’s incident policy. The restore script checks schema version; operators must additionally reconcile data, sequences, ownership, ACLs, IdP configuration, and queue state.

See [database roles](database-roles.md) for runtime/migration separation. Backup and restore roles are separate operator-managed identities; review ownership and ACL policy before recovery. PostgreSQL logical dumps do not include global roles, so recover those separately.

## Recovering the rest of the service

- **OIDC/IdP:** use the IdP vendor’s supported consistent backup/restore procedure, including its database, signing keys, realm/tenant configuration, client credentials, and encryption keys. Restore to a separate IdP first. Confirm issuer, signing-key continuity/rotation, client IDs, redirect URIs, audiences, roles, and administrator recovery before connecting liteCR. Do not assume the application PostgreSQL dump contains IdP state.
- **Configuration and secrets:** recover `/etc/litecr/litecr.env` and `/etc/litecr/migration.env` from the organization’s encrypted configuration/secret manager backup, preserving ownership and modes. The API file contains runtime DB/OIDC settings; the distinct migration file contains `LITECR_MIGRATION_DATABASE_URL` and is readable only by the migration account. Also recover session/signing keys, backup repository password, TLS private keys where locally managed, and trust anchors. Rotate any credentials whose custody or exposure is uncertain. Do not put secret contents in the runbook or diagnostics.
- **Release and UI artifacts:** retrieve the immutable release archive, checksum/signature, SBOM, migration set and matching source/build metadata from the approved artifact repository. Verify its digest/signature before install. Do not rebuild a historical release during an incident unless the reproducible-build evidence and exact dependencies are available.
- **Host/gateway:** recreate OS packages, service account, file ownership, firewall, systemd units, Caddy/TLS configuration, and PostgreSQL access using the matching release installation instructions. Keep PostgreSQL and management ports private.
- **Optional worker:** The release includes `litecr worker` and a gated systemd template; the installer does not install or enable it, and packaged-service acceptance is open. Before database maintenance, establish whether any worker is running. In an approved rehearsal, stop it with the tested command/service procedure and verify queue/lease state; verify in-flight drain and leased-job recovery for the actual service. Stopping the API does not stop a separate worker. Preserve database queue/outbox state through the backup and inspect it only with approved redacted diagnostics; do not delete queue state to make a service appear healthy.

## Incident and recovery record

Record incident ID, operator, UTC start/end, source backup filename/hash, source release/schema/PostgreSQL versions, target release/schema/PostgreSQL versions, key custody confirmation, each recovery action, validation result, exceptions, and decision owner. Keep the actual backup hash and metadata in the restricted incident record, not a public ticket. If integrity, isolation, key access, scope, or schema compatibility is uncertain, stop before connecting the recovered application to users or integrations and escalate to the named service/data owner.

Actual RPO/RTO, retained-snapshot age, transfer throughput, full-restore duration, identity-history reconciliation, and recovery success rate remain **unverified** until a representative authorized drill is run and measured.

Tool behavior references: [PostgreSQL 18 `pg_dump`](https://www.postgresql.org/docs/18/app-pgdump.html) and [Restic repository preparation](https://restic.readthedocs.io/en/stable/030_preparing_a_new_repo.html). These references inform the proposed commands; they are not evidence that this host/repository combination was exercised.
