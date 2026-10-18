# API overview

The API is experimental. Requests require authenticated identity and applicable registry/organization/resource permissions. Cookie-authenticated mutations require the CSRF mechanism. Machine credentials are separately scoped. See [access](contracts/ACCESS.md).

## Core routes

| Method | Path | Purpose |
|---|---|---|
| POST | `/api/v1/source-records` | Accept a source record/version. |
| POST | `/api/v1/person-search` | Exact public-ID or alias search. |
| GET | `/api/v1/registries/{registry_id}/organizations/{organization_id}/persons/{public_id}` | Scoped person detail. |
| GET | `/api/v1/registries/{registry_id}/organizations/{organization_id}/source-records/{source_record_id}` | Resolve a source record's person. |
| POST | `/api/v1/match` | Generate advisory evidence. |
| GET, POST | `/api/v1/review-cases` | List or open review cases. |
| GET | `/api/v1/review-cases/{case_id}` | Review detail with record/assignment checks. |
| POST | `/api/v1/review-cases/{case_id}/assignment` | Assign a case. |
| POST | `/api/v1/review-cases/{case_id}/decisions` | Submit an authorized decision. |

Payloads and validation are defined in the current [registry](../crates/api/src/registry/mod.rs), [matching](../crates/api/src/matching/mod.rs) and [review](../crates/api/src/review/mod.rs) adapters. Merge/reversal actions use review decisions; there are no standalone public merge/reversal routes. Migration and configuration routes have separate authorization and policies; see [migration format](migration/V1-PACKAGE.md).

## Register a synthetic source record

After authentication and scoped access provisioning, send this JSON body to `POST /api/v1/source-records` with `Content-Type: application/json` and an `Idempotency-Key` unique to this logical command. Replace registry and organization values with the scope actually granted to your test identity. Cookie-authenticated requests must also satisfy CSRF validation.

```json
{
  "registry_id": "beta-registry",
  "organization_id": "beta-organization",
  "source_system_id": "synthetic-demo",
  "source_record_key": "demo-person-001",
  "source_version": "1",
  "assertions": [
    {
      "field_path": "name.original",
      "original_value": "ສົມໃຈ ວົງສີ",
      "language_tag": "lo",
      "script": "Lao"
    }
  ],
  "identifiers": []
}
```

Retain the returned `data.person_public_id`, `data.source_record_id` and receipt information. Retry the same logical command with the same idempotency key and body; changing the command under the same key is a conflict. Do not invent an assigning authority merely to make an identifier look authoritative.

## Search by public ID or alias

Send a body such as the following to `POST /api/v1/person-search`. Replace the placeholder with the actual returned public ID. The identity needs explicit `person:search` in this scope.

```json
{
  "registry_id": "beta-registry",
  "organization_id": "beta-organization",
  "public_id_or_alias": "REPLACE_WITH_RETURNED_PUBLIC_ID",
  "limit": 20
}
```

This endpoint performs exact ID/alias lookup. Use the separate matching workflow for demographic candidate comparison. Authentication, CSRF and permission failures remain failures; do not bypass them by assigning broad administrator permissions.

## Browser login

`GET /api/v1/access/oidc/login`, `GET /api/v1/access/oidc/callback`, `GET /api/v1/access/session` and `POST /api/v1/access/logout` are implemented. The session cookie is HttpOnly; the CSRF cookie is intentionally readable and is not the session credential. Use HTTPS and secure cookies outside isolated loopback development. Do not store session credentials in localStorage or bug reports.

## FHIR R4

The authenticated Patient adapter exposes `/fhir/r4/metadata`, `/fhir/r4/Patient` and `/fhir/r4/Patient/{id}`, with `/fhir` aliases. It supports read, exact identifier search and explicitly configured create. See [adapter details](integration/FHIR-R4-BASELINE.md) and [profile notes](contracts/fhir-r4-profile.md). This is not full FHIR coverage or verified OpenCR compatibility.

MCP and a general patient export API are not available in this Beta. See [known limits](BETA.md).
