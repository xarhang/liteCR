# Failure and recovery testing

Run only in disposable synthetic environments. Installer/operations tests use command shims and do not cover every application, network, storage or host failure.

Rehearse API/worker termination, rollback, commit-before-response and idempotent retry; database/IdP unavailability; denied and foreign-scope requests; bounded resource exhaustion; and backup/restore into a separate isolated destination. Check atomic registry/audit/outbox state, lease recovery, no duplicate effects and redacted diagnostics.

Record exact source/package identity, fixture inputs, injected failure, observed status, before/after counts and recovery outcome. Independent-host recovery, fencing and RPO/RTO need separate acceptance. Do not infer a guarantee from a same-host simulation. See [backup/restore](../../docs/runbooks/backup-restore.md) and [Beta limits](../../docs/BETA.md).
