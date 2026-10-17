# ADR-002: First-party PostgreSQL worker queue

Status: implemented for the optional advisory matching worker.

## Decision

Use a first-party PostgreSQL queue for `registry.source_record.accepted` events. Enqueue within the source mutation's outbox transaction, claim bounded work with leases, fence completion by lease and current source state, and atomically record advisory evidence with a per-event receipt.

The worker uses a dedicated restricted database role and cannot mutate Registry identity data. API and worker share the product migration set. Apalis is not a runtime dependency.

## Consequences

liteCR owns lease expiry, retry, deduplication, retention and operational recovery. External event delivery is a separate integration; advisory completion does not promise exactly-once delivery to another system.

The packaged worker service is optional. Configure its separate credentials and enable it only after reviewing the [deployment instructions](../DEPLOYMENT.md) and [database-role runbook](../runbooks/database-roles.md). Failure recovery, capacity and target-host operation require deployment validation. See [Beta limits](../BETA.md).
