# Beta deployment

liteCR is experimental. Use synthetic data in an isolated evaluation environment. A clean-host installation, organizational IdP integration, representative workload, and independent recovery drill still need validation before real patient data or production use.

## Deployment profile

The native installer targets Ubuntu 24.04 x86_64 with systemd and PostgreSQL 18. This is a target profile, not a claim of support for every Linux distribution or PostgreSQL version. Windows scripts are development prototypes; WSL results do not establish native Windows service support.

The API serves the embedded web UI. PostgreSQL, an OIDC provider, and a same-origin HTTPS gateway are managed separately. Node.js and Rust are build dependencies, not runtime requirements. No Redis, Elasticsearch, HAPI server, or external AI service is required. OpenCR compatibility is relevant only when that migration or integration is selected and must be checked against the actual endpoint contract.

## Installation

Follow [the installation guide](../tools/install/INSTALL.md) and [database roles](runbooks/database-roles.md). The installer stages a release; migration and activation are explicit steps. Use a release built from a clean source revision, verify its checksums and provenance, and retain the source, license, dependency notices, and recovery artifacts. A checksum alone does not authenticate the publisher. See [Beta release limits](BETA.md).

Keep runtime and migration credentials separate in protected files. Run the API as an unprivileged account, bind it to loopback behind the gateway, keep PostgreSQL private, and use HTTPS OIDC issuer/origin/redirect URLs with secure cookies. Never disable authentication or TLS to make preflight pass. Do not execute environment files as shell code.

After installation, verify health, OIDC login/logout, scoped access and denial of unauthorized requests, and a synthetic registration/search/review workflow. Test the actual deployment's cookie and gateway behavior. The optional worker is not enabled by the installer; its service identity, permissions, lease recovery, and shutdown behavior need separate verification before use.

## Backup and upgrades

Follow [backup and isolated restore](runbooks/backup-restore.md), [upgrade and rollback](runbooks/upgrade-rollback.md), and [redacted diagnostics](runbooks/redacted-diagnostics.md). Logical database dumps do not include IdP state, secrets, certificates, or host configuration. Protect those separately, retain recovery keys outside the failed host, and rehearse recovery to another isolated host. No RPO, RTO, zero-data-loss, or high-availability guarantee is established.

Stop all writers before migrations that require quiescence, including remote API instances, workers, ingestion clients, and reindex processes. A failed activation does not roll back committed migrations. Never switch to an older binary unless it supports the resulting schema and persisted job formats.

### Schema 29 → 31 marker-partition maintenance

This is a maintenance procedure for the schema-29 to schema-31 upgrade. Rehearse it on an isolated copy before changing a shared database; local synthetic checks do not establish deployment or recovery readiness.

Migration 30 converts only `matching.indexed_source_version` to native `HASH (source_record_id)` partitioning with eight fixed partitions. It takes an `ACCESS EXCLUSIVE` lock, copies complete marker rows, checks bidirectional `EXCEPT ALL`, retains original `indexed_at` values, and preserves the primary key, source-version foreign key, normalizer lookup index and supported parent ACLs including grant options. Authoritative identity/source facts and persisted matching evidence are not repartitioned. Unsupported owners, column ACLs, external grant chains, custom constraints/indexes/storage settings, RLS/policies/triggers/rules, inheritance, replication/publication membership and security labels fail closed. Resolve such configuration through a separately reviewed migration rather than bypassing these guards. This is a quiesced transactional conversion, not an online or zero-downtime upgrade.

1. Verify the exact pending candidate, supported PostgreSQL/version path and original schema-29 artifact/configuration. Protect a current consistent backup and verify its restore into a separate disposable destination before changing the application database. Retain separately recoverable keys and configuration. Measure space for the existing and replacement tables/indexes, temporary work, generated WAL and backup; row count alone does not establish sufficient headroom.
2. Put ingress into maintenance mode and pause every source ingestion/integration writer. Drain and stop **all API instances, matching workers and `litecr matching reindex` processes/schedulers** using this database, including remote hosts and separately invoked commands. Confirm their in-flight transactions have completed. Inspect session PID, role, application name, state and wait information without printing query text or credentials; resolve unexpected sessions instead of indiscriminately terminating clients.
3. The packaged `litecr-migrate.service` conflicts with the local API/worker units and uses the release-operation `flock`. These controls do not stop remote instances, independent reindex commands or external writers. The installer only stages the pending release; it does not prove quiescence. Keep every writer stopped throughout migration and validation.
4. Have the DBA configure and verify bounded `lock_timeout` and `statement_timeout` for the migration connection through reviewed PostgreSQL role/database settings. Choose the bounds from a rehearsal and the maintenance window. The installer accepts only documented migration-environment keys; do not add `PGOPTIONS` to that file. A lock timeout must prompt investigation instead of an unbounded retry. Keep credentials out of command arguments, history, and diagnostic output.
5. Run the pending release through `litecr-migrate.service`. SQLx commits each migration separately; verify the schema metadata and individual migration-30/31 ledger results on completion **or any failure**. Do not infer database state from the process exit code or current release symlink alone. Read-only checks include:

   ```sql
   SELECT version FROM litecr.schema_meta WHERE singleton;
   SELECT version, success FROM public._sqlx_migrations
   WHERE version >= 29 ORDER BY version;
   ```

6. Verify the marker parent is partitioned with eight attached hash partitions; confirm valid PK/FK and normalizer indexes, complete row/timestamp reconciliation and effective ACLs. Reapply and verify the selected deployment's current least-privilege scripts as required. Worker reads through the parent must succeed while direct child access remains denied. Analyze the parent/partitions and inspect the relevant query plans.
7. Activate only a release whose `doctor` passes against the actual schema. Start the matched API/worker versions, execute scoped synthetic ingestion/search/match/replay checks, and inspect errors and queue state before resuming traffic/jobs. The optional worker retains its existing enablement/acceptance requirements. Do not enable it solely because partition migration succeeded.

If migration 30 fails and both schema metadata and the SQLx ledger remain at 29, verify that the original ordinary table, rows and ACLs were restored by transaction rollback. The original schema-29 release may resume only after that verification and diagnosis. If migration 30 committed but migration 31 failed, the database can remain at **schema 30**: the old schema-29 binary is incompatible and must stay stopped. Complete a verified forward retry with a matching release, or follow the tested restore procedure. A schema-31 database likewise cannot be rolled back by switching only the binary symlink; activation failure does not undo committed migrations. Any restore must account for post-backup/post-upgrade writes and the actual backup/WAL recovery limits. No zero-data-loss or accepted recovery objective is inferred from local rollback tests.

## Capacity and evaluation limits

Size the deployment from measured person/source counts, candidate fan-out, concurrent requests, imports, and retention. Synthetic benchmarks are not a production SLA or evidence of real-world matching accuracy. Keep original assertions and identifiers; matching evidence does not authorize automatic merging. Validate matching quality on appropriately governed labeled data before operational use.

Before production, validate the selected host and IdP/gateway configuration, authorization boundaries, measured load and soak behavior, migration/recovery drills, translations/accessibility, and the exact release and dependency notices. Beta publication does not close these checks.
