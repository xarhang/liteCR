# Operator client-registry migration

This runbook describes the current source-only `litecr-migration/v1` workflow. It has been exercised with synthetic data only. Do not use it to import an OpenCR/client export until the source profile, identifier/relationship semantics, client compatibility and reconciliation plan have been approved and rehearsed in an authorized staging environment.

## Prepare the package offline

Use a protected operator workstation with Python 3.11+, the release's `share/tools/migration/litecr_migrate.py`, the exact source manifest and stream, and a private staging directory. The stager has no database access. First validate the inputs, then create deterministic batches no larger than the API's 8 MiB raw-batch bound:

```sh
python3 share/tools/migration/litecr_migrate.py validate \
  --manifest /secure/source/manifest.json \
  --stream /secure/source/records.ndjson \
  --work-dir /secure/stage/validation

python3 share/tools/migration/litecr_migrate.py stage \
  --manifest /secure/source/manifest.json \
  --stream /secure/source/records.ndjson \
  --output /secure/stage/package \
  --batch-size 500 \
  --batch-bytes 8388608
```

Review `checkpoint.json` and `quarantine.json` without copying source values into ordinary logs. The current Rust importer accepts a package only when every manifest row is represented exactly once: any quarantine or exact-replay duplicate stops apply. Historical `link` and `non_match` rows remain unsupported and must not be silently discarded. Preserve the source package and stage artifacts under the data custodian's retention policy.

## Review quarantines and prepare a resubmission

Never edit the source manifest, stream, or stage directory. For format-only errors, prepare a reviewed local corrections JSON whose entries identify the exact quarantined line and corrected row, for example `[{"line":4,"row":{...}}]`. The helper accepts only mechanical row-format codes, verifies the source report/digests, writes a distinct derivative manifest/stream, and emits a hash-only API request. Stage that derivative separately. The correction package must retain every non-corrected physical source line exactly.

```sh
python3 share/tools/migration/litecr_quarantine.py prepare \
  --action corrected_resubmission \
  --manifest /secure/source/manifest.json \
  --stream /secure/source/records.ndjson \
  --stage-dir /secure/stage/original \
  --corrections /secure/review/corrections.json \
  --output /secure/stage/derived-package \
  --reason "reviewed source encoding repair CR-1234" \
  --idempotency-key "CR-1234-quarantine-v1"

python3 share/tools/migration/litecr_migrate.py stage \
  --manifest /secure/stage/derived-package/manifest.json \
  --stream /secure/stage/derived-package/records.ndjson \
  --output /secure/stage/derived

litecr migration disposition \
  --request-file /secure/stage/derived-package/disposition-request.json \
  --manifest /secure/source/manifest.json \
  --stream /secure/source/records.ndjson \
  --stage-dir /secure/stage/original \
  --replacement-manifest /secure/stage/derived-package/manifest.json \
  --replacement-stream /secure/stage/derived-package/records.ndjson \
  --replacement-stage-dir /secure/stage/derived \
  --credential-file /secure/operator/litecr-migration-write.credential \
  --api-base-url https://litecr.example.org
```

The CLI locally checks original quarantine line/code/hash evidence and proves that the derived stream changed only the selected correction lines. The API records action, actor, exact target scope, reason, parent/replacement manifest hashes, idempotency fingerprint and row hashes atomically with audit/outbox metadata. It receives no patient/source values. Repeating the exact request returns the same receipt; changing the request under that key conflicts.

`duplicate_acknowledged` is permitted only when every flagged replay is byte-for-byte identical to its referenced first occurrence and there are no quarantined rows. Its derivative removes only those duplicate physical lines; the CLI compares all retained lines byte-for-byte. `deferred` records unresolved quarantines without a replacement manifest. Neither deferred rows nor historical relationships, same-version payload conflicts, semantic errors or database identity conflicts can be made importable by disposition. They require a separately modeled and reviewed solution. Recording a disposition never applies the derivative; the replacement still runs through normal no-write validation and apply.

## Apply or resume

Provision an opaque migration-operator credential through the approved access-administration process, scoped to the manifest's target registry and organization. Use `migration:read` for package validation, checkpoint/status inspection, and reverse preflight; it cannot apply batches, record quarantine dispositions, or reverse imported sources. Use `migration:write` only for an operator who must perform those mutations; write implies read so the CLI can validate and resume. Ordinary `SourceWrite` (`source:write`) does not authorize migration operations. Store the credential alone in a regular file with mode `0600` on Unix; never put it in command arguments, shell history, logs, or source control. The CLI sends it only as HTTPS bearer authorization (plain HTTP is accepted only for loopback development).

Calculate the digest over the exact manifest bytes and confirm it on the apply command. The CLI rechecks the complete stream, stage checkpoint, quarantine, each batch and sidecar, then calls the authenticated no-write validation route for every batch before committing the first batch. It checks the server checkpoint and resumes only at the next authoritative sequence. A later database collision can still stop a later commit; committed batches remain journaled and the partial manifest may be reversed if the server's whole-set dependency checks allow it.

```sh
manifest_sha256="$(sha256sum /secure/source/manifest.json | cut -d ' ' -f 1)"
litecr migration apply \
  --manifest /secure/source/manifest.json \
  --stream /secure/source/records.ndjson \
  --stage-dir /secure/stage/package \
  --credential-file /secure/operator/litecr-migration-write.credential \
  --api-base-url https://litecr.example.org \
  --confirm-manifest-sha256 "$manifest_sha256"

litecr migration status \
  --manifest /secure/source/manifest.json \
  --credential-file /secure/operator/litecr-migration-write.credential \
  --api-base-url https://litecr.example.org
```

Do not rerun with a byte-different manifest. A package conflict, scope denial, validation failure or unclear checkpoint blocks further writes until the owner reviews the redacted status and audit evidence.

## Reverse source effects

Reverse is a compensating source-withdrawal command, not restoration of an old database snapshot. It withdraws only source rows journaled for the exact manifest; it preserves shared people, audit history and aliases, and detaches identifiers only when ownership is proven. Any later source change or identity dependency blocks the whole reversal. A blocked reversal needs an owner-approved forward correction.

Review the exact manifest digest and target scope, then pass the digest again as explicit confirmation and give an incident/change reason:

```sh
litecr migration reverse \
  --manifest /secure/source/manifest.json \
  --confirm-manifest-sha256 "$manifest_sha256" \
  --reason "approved change record CR-1234" \
  --credential-file /secure/operator/litecr-migration-write.credential \
  --api-base-url https://litecr.example.org
```

Retain the returned aggregate receipt with the restricted change record. Never restore an older database backup over newer writes as a migration rollback.

## Current limits

- Evidence is synthetic; no real legacy export, full count reconciliation, deployed OpenCR/client lookup, live migration or cutover has been tested.
- The adapter preserves only the supported source-only subset. Explicit historical relationships are refused; country/source-specific field semantics require a reviewed adapter.
- The stager and CLI require separate operator-workstation tooling. The release bundles the Python stager, and Python 3.11+ must be present on the staging workstation.
- Database migrations still use the separate `litecr-migrate` service credential. The migration API/CLI never connects directly to PostgreSQL.
