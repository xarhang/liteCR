# FHIR R4 adapter baseline

The authenticated Beta adapter implements a narrow Patient boundary. External-client interoperability and general FHIR conformance are unverified. See [API](../API.md), [wire profile](../contracts/fhir-r4-profile.md) and [Beta limits](../BETA.md).

The adapter uses FHIR R4 4.0.1 JSON and is deliberately limited to `Patient`. Every response is marked `application/fhir+json`, `Cache-Control: no-store`, and `X-FHIR-Version: 4.0.1`. Error bodies use `OperationOutcome`. Client bindings are server-owned and keyed by `issuer#subject`; when an integration identity has no issuer, its key begins with `#`. The request cannot choose its registry, organization, or source system.

| Interaction | Result | Authorization / boundary |
|---|---|---|
| `GET /fhir/r4/metadata` | CapabilityStatement for R4 4.0.1 and Patient. Create appears only when enabled in that client’s binding. | Authenticated integration-client identity required by composed protected router. |
| `GET /fhir/r4/Patient/{id}` | Scoped read by liteCR public ID; emits `Patient.id` and `meta.versionId`. | `PersonRead`; registry and organization come from the client binding. |
| `GET /fhir/r4/Patient?identifier={system}\|{value}` | FHIR searchset Bundle for an exact assigning-authority/value pair. No demographic/fuzzy search. Result bound is 100; a full bound is rejected as too costly rather than silently truncated. | `PersonSearch`; registry and organization come from the client binding. |
| `POST /fhir/r4/Patient` | Optional create, disabled unless the client binding sets `create_enabled`. Requires `Idempotency-Key` and `X-Source-Record-Key`; maps only the strict Patient input profile into normal Registry ingestion. | `SourceWrite` plus `PersonRead` for the returned representation. Scope and source system are server-owned. |
| Update, patch, delete, conditional create, batch/transaction, history, `$match`, merge, link | Unsupported and not advertised. | Corrections and identity decisions remain normal Registry/Review use cases. |

Create accepts identifiers with `system`, `value`, and `type`; the assigning authority is kept as the identifier system, and the FHIR `CodeableConcept` type is retained as compact JSON. `meta.versionId` is accepted as the upstream source version on create, while reads return liteCR’s person resource version in that field. Supported source assertions are `name` (as a lossless structured assertion), partial or full `birthDate`, standard FHIR gender, `active`, bounded `telecom`, and bounded `address`. Unknown resource properties are rejected by the strict input profile; invalid or unrepresentable internal assertions produce an `OperationOutcome` rather than being silently discarded.

The adapter currently uses a required non-standard `X-Source-Record-Key` header to supply the stable upstream source key. A deployed-client profile must decide whether this header is acceptable or define a versioned mapping. A source-specific profile must verify identifiers, authentication, source-key mapping and request/response behavior against the intended client. OpenCR replacement compatibility is not established. Synthetic fixtures and tests do not establish deployed interoperability or general FHIR conformance.

Migration `0008_integration_delivery.sql` adds a durable, deduplicated delivery-tracking table for outbox event/recipient pairs. It is storage groundwork only: recipient configuration, event disclosure policy, dispatch/lease worker and actual correction-event delivery are not wired in this Beta.
