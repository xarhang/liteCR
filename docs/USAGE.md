# Try the Beta with synthetic records

Complete [local setup](BETA-SETUP.md) first. Use an isolated database, an OIDC test user and grants limited to your test registry and organization. Screen labels may differ with the selected interface language.

## Sign in and select scope

Open the configured application origin, sign in through your IdP and select the registry/organization granted to your identity. Successful IdP login does not automatically give registry permissions. If access is denied, have the operator check the exact issuer, subject, active identity and scoped grant; do not bypass the denial with broad administrator permissions.

## Register an invented source record

Open registration. Use an invented name, a test source-system ID, a unique source-record key and a source version. Preserve the original-script spelling. Add only synthetic identifiers with their assigning authority and type; the same text from different authorities is not automatically the same identifier.

Submit once and inspect the server response. Keep the returned person public ID and source-record ID. If the response is uncertain, check the result before submitting another record. Reusing a source key/version has ingestion semantics; it is not a way to create a second independent person. See the [registry adapter](../crates/api/src/registry/mod.rs) for the current request contract.

## Search and request matching

Use the returned public ID or alias in person search. This search does not accept a name query. It also requires `person:search` in the selected scope.

For demographic candidate evidence, use the matching workflow. Try invented examples with a typo, partial name and two different people sharing a surname. Inspect the evidence and conflicting identifiers, dates and source assertions. A high score is not proof that two people are the same; synthetic examples do not establish population accuracy. Twins and family members must retain separate identities unless an authorized, evidence-backed review determines otherwise.

## Review

A reviewer needs the relevant scoped grant and case assignment. Open the review case and compare source facts before submitting a decision and reason. The server checks record versions; refresh and reassess when a conflict is reported. Merge and reversal are review decisions with retained provenance. Do not merge merely to clear a queue or demonstrate the interface.

## Check access and sign out

Using a second ungranted synthetic scope, confirm that protected records and actions are denied. Sign out, then verify protected actions require authentication again. This is a basic evaluation checklist, not a complete security assessment.

For a bug report, include the commit/version, invented inputs, expected/actual result and redacted error. Never share session cookies, tokens, database URLs or patient data. See [security reporting](../SECURITY.md) and [Beta limits](BETA.md).
