# Upgrade and rollback operations

Rehearse upgrades and recovery on an isolated target using synthetic data. This procedure does not establish a validated production upgrade or rollback path.

## Preconditions

1. Identify the exact current and target immutable release IDs, binary/archive digests, SBOMs, schema versions/ranges, supported PostgreSQL major, configuration format, and any job/event schema versions. Run `litecr version --json` to record `application_version` and `schema_version`; retain the immutable release ID and digest from the approved artifact record because version JSON does not identify an archive digest.
2. Read the target release notes and migration plan. Confirm forward migrations are additive or otherwise explicitly reversible. Keep API runtime settings in `/etc/litecr/litecr.env` and the distinct migration credential in `/etc/litecr/migration.env` as `LITECR_MIGRATION_DATABASE_URL`. Only the one-shot migration service uses that credential; the API never runs migrations at startup. Ensure only one migration process can run.
3. Verify a recent, checksummed PostgreSQL backup and separately recoverable Restic/IdP/config/artifact keys. Test restore on a separate isolated target before production operations depend on that snapshot.
4. Check disk headroom, backup age, DB health, migration lock, outbox delivery and active integration status using approved redacted diagnostics. Keep the change owner, database operator and rollback decision owner available.
5. Decide which writes/integrations are paused or permitted during the change. liteCR now has an optional worker command and gated systemd template; the installer does not install or enable it, and packaged-service operation is not accepted. Confirm whether a worker is running. If enabled under an approved rehearsal, stop it using its approved service procedure and confirm queue/lease state; the command drains an in-flight lease on shutdown in unit coverage, but packaged-systemd drain/recovery is unverified. Stop the API gracefully through systemd and inspect persisted outbox/job state; stopping the API alone does not stop a separately running worker.

## Controlled upgrade

Take and verify the pre-upgrade backup; record UTC time and metadata. Stop or isolate API writers according to the approved maintenance plan, preserving the database and outbox state. Install the target release side-by-side. Installation moves only `/opt/litecr/pending`; it leaves `/opt/litecr/current` on the active immutable release. The one-shot migration unit runs the pending candidate and serializes with installer/activation operations. After migration succeeds, `litecr-activate.service` requires that migration unit, runs the candidate's `litecr doctor` against the runtime database, and atomically moves `current` only after exact schema compatibility is confirmed. Then start the API on first install or restart it for an upgrade. Keep the old immutable release and pre-upgrade backup available. Re-enable integrations/writers only after smoke checks of authenticated health, scoped synthetic lookup/write where authorized, outbox progress and audit continuity. Record all new writes after cutover.

If the host reboots before migration, the previous active release remains selected. If it reboots after a schema change but before activation, API startup checks the exact schema version before composing routes or binding its listener; an incompatible active binary exits. A migration or candidate doctor failure leaves `current` unchanged. The installer only replaces a changed packaged systemd unit when that file still exactly matches the active release's shipped template; local unit edits stop the upgrade for operator review. The installer, migration, and activation commands share a lock to prevent their release-pointer operations from overlapping.

Never run migrations implicitly on API startup or start two migration processes concurrently. The optional worker unit remains gated; do not install or enable it outside an approved test until packaged service and recovery gates pass. Do not delete PostgreSQL directories or unknown volumes as a cleanup step.

## Rollback decision

First determine whether the old binary supports the already-upgraded schema and every persisted job/event format. If yes, and compatibility is documented and tested, a binary-only rollback may be possible while preserving all post-upgrade writes. If not, do not point the old binary at the upgraded database.

A pre-upgrade database restore discards writes committed after that backup. It is not a safe default rollback after accepting new registrations, edits, reviews, merges, imports, or integration messages. Prefer a forward fix or a tested compensating/replay procedure that preserves new writes. If a restore is the only recovery, pause all writers, preserve the current database and post-cutover change evidence, obtain the data owner’s decision, restore to an isolated target first, reconcile every post-backup change, then plan a controlled cutover. Never overwrite the current database as an exploratory step.

If migration compatibility, job replay, alias/relationship history, or post-cutover write reconciliation cannot be proved, stop and escalate for a forward-recovery plan. Do not invent reverse SQL or silently rematch records.

## Completion record

Record release and schema before/after, migration result, DB backup hash, downtime, queue state, counts/reconciliation, smoke checks, errors and decision owner. Recovery/rollback time and data-loss windows are unknown until an isolated representative exercise measures them.
