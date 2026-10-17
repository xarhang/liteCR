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

## Browser login

`GET /api/v1/access/oidc/login`, `GET /api/v1/access/oidc/callback`, `GET /api/v1/access/session` and `POST /api/v1/access/logout` are implemented. The session cookie is HttpOnly; the CSRF cookie is intentionally readable and is not the session credential. Use HTTPS and secure cookies outside isolated loopback development. Do not store session credentials in localStorage or bug reports.

## FHIR R4

The authenticated Patient adapter exposes `/fhir/r4/metadata`, `/fhir/r4/Patient` and `/fhir/r4/Patient/{id}`, with `/fhir` aliases. It supports read, exact identifier search and explicitly configured create. See [adapter details](integration/FHIR-R4-BASELINE.md) and [profile notes](contracts/fhir-r4-profile.md). This is not full FHIR coverage or verified OpenCR compatibility.

MCP and a general patient export API are not available in this Beta. See [known limits](BETA.md).
