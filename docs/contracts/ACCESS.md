# Access and scope

Protected API and application operations require authenticated identity, explicit permissions and permitted registry/organization scope. A role name or a field supplied by a client cannot expand access. Deployment IdP configuration, privacy policy and operational acceptance remain separate; see [Beta limits](../BETA.md).

## Server-derived context

The access layer constructs an `AccessContext` with actor identity, actor kind, grants, resource scope and request attribution. Empty scope grants no access. Resource IDs, search filters and registry/organization fields are selectors checked against that context. Interface visibility is not an authorization boundary.

Permissions distinguish source ingestion, person read/search, matching, review and configuration operations. Identity decisions retain their version, assignment and conflict checks. Country, nationality, language and requested FHIR profile do not grant access. No cross-registry sharing is implied by multilingual support.

## Browser sessions

The browser uses OIDC authorization-code login and a same-origin backend session. The session cookie is HttpOnly and uses SameSite=Lax; deployed HTTPS requires Secure cookies. A separate CSRF cookie is deliberately readable by the interface for double-submit protection. Do not treat that CSRF value as an authentication token or place authentication secrets in browser local storage.

The API exposes login, callback, session and logout routes under `/api/v1/access`. OIDC and session validation do not establish that a particular organization's provider, claim mappings, MFA or revocation behavior works. Validate those against the intended IdP and gateway before using sensitive data.

## Integration and worker identities

Machine callers use configured, scoped identities rather than browser-user impersonation. FHIR bindings select server-owned registry, organization and source-system values; see [FHIR boundary](../integration/FHIR-R4-BASELINE.md). Do not assume support for an arbitrary provider's OAuth client-credentials flow.

The optional advisory worker has a separate restricted database identity. API, migration and worker database credentials are distinct; see [database roles](../runbooks/database-roles.md). MCP transport and registry-data tools are not available in this Beta.

## Migration permissions

`migration:read` permits no-write validation, checkpoint inspection and reverse preflight within its assigned scope. `migration:write` implies read and is required for apply, quarantine disposition and source withdrawal. `source:write` alone grants no migration authority. See [migration package contract](../migration/V1-PACKAGE.md).

## Audit and transaction requirements

Registry mutations authorize the command and recheck versions and constraints inside the transaction that commits the mutation, required audit/outbox records and idempotency outcome. External network calls do not belong in that transaction. Retry does not preserve revoked permission or bypass current authorization.

Keep tokens, credentials and demographics out of ordinary operational logs. Audit and source history are protected records, not public diagnostics. Data controllers must define retention, permitted organizations, disclosure purpose, grant ownership, sensitive-field access and administrator recovery. Local authorization tests do not establish deployment security or legal compliance.
