# Redacted diagnostics and backup failure response

Operational diagnostics must exclude patient data and credentials. `litecr version --json` prints `application_version` and `schema_version` without DB access, and `litecr doctor` uses fixed redacted errors. The API reads `/etc/litecr/litecr.env`; the one-shot migration service separately reads `/etc/litecr/migration.env` containing `LITECR_MIGRATION_DATABASE_URL`. Do not invent a shell wrapper that dumps environment/configuration.

## Safe collection

Use the installed release’s `litecr version --json` and `litecr doctor`. Capture command exit status, UTC time, application/schema versions, immutable release ID from the artifact record, unit state, disk free space, backup artifact metadata, and relevant service journal lines only through the organization’s approved restricted channel. `doctor` emits fixed redacted errors including `LITECR_DATABASE_URL is required`, `database connection failed; verify service and credentials`, `could not read liteCR schema metadata; run litecr migrate`, and `liteCR schema is not installed; run litecr migrate`; an incompatible-schema message includes only schema version values. Do not collect environment dumps, config contents, command-line arguments, SQL statements with values, HTTP request bodies, authorization headers, OIDC tokens, connection URLs, patient rows, raw dump names with identifiers, or PGPASS/Restic password files in general logs.

`doctor` also reports whether `synchronous_standby_names` is configured, without printing standby names. “Policy configured” is not proof that any standby is connected, caught up, independent, fenced, or recoverable; consult the approved topology and live database monitoring. If no policy is configured, commits rely on local WAL flush only, which does not protect against host/storage loss.

The backup and restore scripts emit stable error codes without shell command output. Preserve the original exit code and code-only stderr. Their backup manifest contains database and version metadata but no patient records; treat the dump and all related files as sensitive registry data. Hashes identify an artifact but do not validate that its contents are recoverable.

## Common failures

| Diagnostic code | Safe response |
|---|---|
| `CONFIG_*`, `SECRET_FILE_*` | Check ownership, mode, path and allowed key names locally without copying secret contents into tickets. |
| `INSUFFICIENT_FREE_SPACE` | Stop retries, identify the approved backup filesystem, free only known disposable files under retention policy, then rerun. Never remove unknown volumes or live PostgreSQL files. |
| `BACKUP_ALREADY_RUNNING` | Check the service/process owner and lock holder; do not remove a lock while a process is active. |
| `PG_DUMP_FAILED`, schema/version query codes | Check PostgreSQL availability and role grants through restricted admin procedures. Do not enable verbose SQL/credential tracing in general logs. |
| `RESTIC_BACKUP_FAILED` | Check destination reachability and key custody via approved secret channels. Do not switch to an unapproved repository or send an archive elsewhere. |
| `DUMP_CHECKSUM_MISMATCH`, `ARCHIVE_INVALID`, `POSTGRES_MAJOR_MISMATCH` | Quarantine the artifact and stop restore. Retrieve a verified backup or provision the matching supported target major. |
| `RESTORE_HOST_MUST_BE_LOCAL`, `LIBPQ_REMOTE_OVERRIDE` | Verify target-cluster isolation and remove connection-service overrides; never bypass these guards to point at a production host. |
| `TARGET_DATABASE_ALREADY_EXISTS` | Choose a new unused isolated database name; inspect the existing database without dropping it. |

Escalate suspected credential exposure to the IdP/database/service owner and rotate through the approved process. Escalate possible data integrity issues to the registry/data owner. Record evidence in the restricted incident system with the minimum necessary metadata.
