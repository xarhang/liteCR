# Architecture

liteCR is a Rust modular monolith with an embedded React interface and PostgreSQL persistence. The native Linux deployment uses separate API and optional worker processes from the same executable, behind a same-origin HTTPS gateway. See [deployment](DEPLOYMENT.md), [API](API.md) and [Beta limits](BETA.md).

## Components

| Component | Responsibility |
|---|---|
| HTTP adapters | Request validation, authentication/session boundary and response mapping. |
| Application services | Authorization, use-case orchestration and transaction boundaries. |
| Domain | Identity, matching and review rules independent of HTTP and SQL. |
| PostgreSQL infrastructure | Scoped repositories, versioned evidence, audit, outbox and advisory worker queue. |
| React interface | Registration, search, review and scoped configuration workflows. |

The backend uses Axum, Tokio and SQLx. CPU-bound matching runs on a bounded blocking executor. Candidate retrieval and active matching concurrency are bounded separately from request acceptance and database connection capacity. Resource settings require measurement against the intended workload.

## Identity and transaction boundaries

Registry owns source records, identifiers, person memberships, aliases and identity mutations. Original assertions, scripts, date precision and assigning-authority identifiers remain available with provenance. Matching produces versioned advisory evidence; a candidate score cannot merge people.

Review decisions check current evidence and record versions before invoking Registry mutations. Merge preserves aliases and history. Reversal is a compensating operation with dependency checks, rather than a database snapshot restore.

Registry mutations commit their required audit, outbox and idempotency records in the same transaction. External calls do not run inside that transaction. Worker leases and receipts support retry-safe advisory processing; they do not establish exactly-once external delivery. See [worker queue decision](adr/002-first-party-postgres-worker.md).

## Access and integrations

Protected use cases receive server-derived permissions and registry/organization scope. Browser authentication uses OIDC and a backend session. A configured IdP and deployment-specific validation remain necessary; see [access contract](contracts/ACCESS.md).

The [FHIR adapter](integration/FHIR-R4-BASELINE.md) provides a narrow Patient interface. Source migration uses an explicit [package format](migration/V1-PACKAGE.md). Neither establishes compatibility with an external deployment. MCP transport and registry-data tools are unavailable in this Beta.

## Configuration and operations

Localization catalogs, appearance and country profiles have separate versioned configuration workflows. Language and appearance never change matching semantics or access scope. See [theme requirements](THEMES.md).

PostgreSQL is authoritative; the Beta does not use an application memory index for candidate discovery. Backups, recovery, HTTPS and IdP operation are deployment responsibilities. Local tests do not establish production availability, representative matching accuracy or supported capacity. See [Beta requirements](BETA.md) and [operator runbooks](runbooks/backup-restore.md).
