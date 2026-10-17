# liteCR migration package v1

The package contract covers the Python offline validator/stager, protected migration API, `litecr migration` CLI and quarantine disposition. It does not identify an OpenCR export format or establish live-source compatibility. Use synthetic data for Beta evaluation; see [Beta limits](../BETA.md).

## Package files and bounds

The package is two files: a UTF-8 JSON manifest (at most 1 MiB) and a UTF-8 NDJSON stream (at most 20 GiB). Each physical line is exactly one source-record row and may not exceed 1 MiB. The manifest has exactly these fields:

```json
{
  "format": "litecr-migration/v1",
  "snapshot_id": "synthetic-snapshot-id",
  "source_system_id": "synthetic-source",
  "target_registry_id": "synthetic-registry",
  "target_organization_id": "synthetic-organization",
  "exported_at": "2026-10-08T00:00:00Z",
  "record_count": 0,
  "stream_bytes": 0,
  "stream_sha256": "<64 lowercase hex characters>"
}
```

`stream_sha256` covers the exact stream bytes, including line feeds. The checkpoint `manifest_sha256` covers the exact manifest bytes, including whitespace and final newline. A retry with semantically equivalent but byte-different manifest is a different run and is refused when an output checkpoint already exists. No self-referential checksum is placed inside the manifest.

Each row includes `source_system_id`, `source_record_key`, and `source_version`; optional `public_id` and `cruid`; arrays `assertions`, `identifiers`, and `relationships`. The row is kept losslessly as JSON and its canonical JSON SHA-256 is used for same-key/version collision checks. Canonicalization sorts object keys by Unicode code point, emits compact UTF-8 JSON with non-ASCII characters unescaped, and permits only integer numbers in the interoperable `[-(2^53-1), 2^53-1]` range. Fractional, exponent-form, non-finite, and out-of-range numbers are quarantined so Python and Rust cannot disagree on the digest. Assertions retain `field_path`, exact `original_value`, optional `language_tag`, `script`, and `partial_date_precision` (`unknown`, `year`, `month`, or `day`). Identifiers retain authority, type, value, and authoritative status. Relationships are explicit `link` or `non_match` facts with target source-system ID and source-record key. Additional source fields are accepted and preserved in the original row rather than mapped or discarded, subject to the canonical-number bound.

This is a liteCR package format, not a source-system export specification. Real export field names, CRUID semantics, language tags, relationship semantics, source-version ordering, and date calendars require a reviewed source-specific mapping. A row or relationship that cannot be represented safely must be quarantined; there is no fuzzy matching or inferred identity relationship in this path.

## Offline validation and staging

Run with Python 3.11+ and no database access:

```sh
python3 tools/migration/litecr_migrate.py validate \
  --manifest /secure/staging/manifest.json \
  --stream /secure/staging/records.ndjson \
  --work-dir /secure/staging/validation

python3 tools/migration/litecr_migrate.py stage \
  --manifest /secure/staging/manifest.json \
  --stream /secure/staging/records.ndjson \
  --output /secure/staging/litecr-stage \
  --batch-size 500 \
  --batch-bytes 8388608
```

The tool hashes and sizes the whole stream before reading a record or creating staged batch files. It then parses bounded lines into a local SQLite validation index so the full NDJSON does not need to be held in memory. It checks duplicate payloads by `(source_system_id, source_record_key, source_version)`, quarantines changed same-version payloads and dangling/contradictory explicit relationships, and never joins similar demographics. Only byte-identical physical rows count as idempotent replays; a same-key/version row with different physical bytes is quarantined even when its JSON value is equivalent. Quarantine output exposes line number, stable error code and SHA-256 of the exact physical row bytes; it intentionally omits row values and keys. `replays.json` separately records replay lines and their first accepted physical line.

Output contains `checkpoint.json`, `quarantine.json`, `replays.json`, `.validation/index.sqlite3`, and `batches/NNNNNNNN.ndjson` with a digest/count/range sidecar. The Python stager caps a batch at 500 records and 64 MiB; the Rust API/CLI transport cap is 8 MiB, so operator packages must use `--batch-bytes 8388608` or less. `--batch-size` and `--batch-bytes` reject values above their applicable caps, and callers can lower either. Output and SQLite working directories are permission-restricted on POSIX; batch and checkpoint writes use temporary-file, fsync, and atomic replace. The local checkpoint advances only after a complete batch and sidecar are durable. On retry, the tool revalidates the complete package and reuses staged batches only when both sidecar metadata and batch-file digest match; an incomplete/mismatched batch is atomically rewritten. A checkpoint bound to another input digest or batch limit fails closed.

`checkpoint.json` records `committed_batch_sequence: 0` because local staging is not an import commit. PostgreSQL `migration_checkpoint.committed_batch_sequence` advances only inside the same transaction as Registry writes, audit/outbox and journal receipts. The Rust CLI accepts only packages with zero quarantined rows and zero exact-replay duplicates; a staged partial subset is fail-closed, not a completed migration.

## Quarantine disposition and resubmission

The original manifest, stream, and stage directory are immutable evidence. `tools/migration/litecr_quarantine.py` writes a separate derivative package and a hash-only disposition request; it never edits the source package. The helper supports three explicit actions:

- `corrected_resubmission` is limited to mechanically recoverable `ROW_INVALID_UTF8_OR_JSON`, `ROW_MISSING_LINE_FEED`, `ROW_TOO_LARGE`, or `NON_CANONICAL_NUMBER` rows. A local corrections JSON must cover every quarantined line. Corrected rows must pass the existing row validator and contain no relationships. Exact byte-identical replay lines in the same package are separately proven and removed in the same derivative, so a mixed mechanical-quarantine/replay package has a complete disposition path. The derivative keeps every other physical source line byte-for-byte.
- `duplicate_acknowledged` applies only to byte-identical physical replays when there are no quarantined rows. The derivative omits only those exact replay occurrences; the CLI rechecks every retained row and the duplicate's exact bytes against the referenced original line.
- `deferred` records unresolved quarantine evidence with no replacement package. It is not approval, removes no rows, and cannot be applied.

Historical links/non-matches (`HISTORICAL_RELATIONSHIPS_UNSUPPORTED`, dangling or contradictory relationships), same-version changed-payload conflicts, semantic row errors and identity conflicts remain fail-closed/deferred. A physical source row is bounded at 64 MiB while staging/disposition evidence is collected; larger rows abort staging rather than being split into false records. Rows above the 1 MiB import limit and within that evidence bound can be mechanically replaced. A disposition may not strip a relationship, suppress a changed source row, infer a match, or auto-approve a source value. The server receives hashes and line/code metadata only; source bytes remain in the protected local work area. Authenticated actor identity and `MigrationWrite` scope come from access middleware, never from request fields.

Schema 0012 stores the action, parent manifest hash, optional distinct replacement manifest hash, exact scope, actor, reason, idempotency hash, request fingerprint and per-line original/corrected hashes (plus duplicate target line where applicable). The disposition row, evidence rows, mutation audit and metadata-only outbox event commit atomically. Identical retries return the same durable receipt; reusing an idempotency key with changed content conflicts. The CLI rechecks the original manifest/stream/stage evidence, the replacement package and the line-level byte relationship before sending the hash-only request. Recording a disposition does not itself import rows; a replacement package must still pass ordinary stage, no-write validation, apply, status and reconciliation.

Example: stage the original package first, prepare reviewed corrected rows locally, then stage the separate derivative:

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

For `deferred`, omit replacement package flags. For duplicate acknowledgement, use `--action duplicate_acknowledged`, omit `--corrections`, and stage the derived manifest/stream before calling the CLI. Retain the returned receipt with the restricted change record.

## Application and durable journal

After offline staging, the thin authenticated CLI verifies the staged manifest/checkpoints/sidecars and package integrity, then POSTs every batch to the protected server no-write route `/api/v1/migrations/batches/validate` before it sends any batch to commit. The route is mounted behind access middleware and requires `MigrationRead` (`migration:read`) for the exact registry/organization; `migration:write` implies read, while `SourceWrite` alone is insufficient. Validation performs payload/mapping checks without creating a migration checkpoint or writing Registry state. Only after all staged batches validate does the CLI apply batches and resume from the server checkpoint, which requires `MigrationWrite`. Database-dependent identity conflicts remain commit-time checks; therefore a later batch can still fail after earlier batches committed, at which point status/resume or safe source-only reversal is used under its own checks.

`crates/application/src/migration` validates batch scope, maximum 500 records, per-row size, unique key/version, exact concatenated bytes and batch SHA-256 before calling the `MigrationExecutor` port. The executor contract requires the existing authorized Registry use case and the migration metadata to commit in one SQL transaction. The infrastructure journal API in `crates/infrastructure/src/migration` enlists checkpoint, batch and original-row journal inserts into a caller-owned transaction; it is not a source-record writer and must not be called as a substitute for Registry mutations.

The migration journal retains exact manifest and stream digests, batch sequence/digest, source key/version/payload digest, original NDJSON, preserved public ID/CRUID, resulting source/public references and expected state for reverse checks. Alias/identifier effects retain introducing-batch provenance for partial reversal. Quarantine disposition stores hash-only line evidence. The manifest hash is the run key. Batch sequences must be contiguous. A retry of an already committed sequence succeeds only when its batch digest and journal receipts match; otherwise the checkpoint conflicts.

**Current acceptance boundary:** API/CLI source calls the migration service and executor. The CLI exposes `migration apply`, `status`, `reverse`, and `disposition`; only zero-quarantine/zero-exact-duplicate packages are accepted for apply. A disposition is a separate audited action and never bypasses normal package validation. Unsupported historical relationships remain fail-closed. Real source mapping, independent source reconciliation, external-client compatibility and production operation require separate validation.

## Reverse replay

Rollback is **source-only manifest withdrawal**, not restoration of a database snapshot. The protected reverse route/CLI identifies the exact manifest and requires the same manifest digest as explicit confirmation plus an operator reason. It withdraws only committed source assertions/versions recorded in that manifest's journal, including when the manifest is partially applied; it preserves people and immutable history, and returns an idempotent durable receipt. Schema 0011 ties alias/identifier effects to batch provenance for conservative partial reversal. Prechecks refuse changed/later source state and known downstream dependencies; a refusal blocks the whole requested manifest withdrawal. It never restores a database snapshot or erases unrelated post-import writes.

The offline CLI `reverse-check` remains a separate historical source-state precheck. It accepts an NDJSON current-state snapshot carrying `current_receive_version` and a durable journal NDJSON with `expected_current_payload_sha256` and `expected_receive_version`; the current digest excludes only the `current_receive_version` metadata property. Passing that precheck alone does not execute the protected reverse operation.

Passing the offline precheck only means the source state is unchanged and representable under the supplied snapshot. Any intervening edit, merge/link dependency, missing row, or unsupported relationship blocks source withdrawal; operators must use a separately reviewed forward correction when reversal is unsafe. Restoring an old backup is not rollback.

## Beta validation boundary

Exercise staging, no-write validation, apply, status, retry and reversal against an isolated synthetic database before evaluating a source-specific mapping. Reconcile source counts and every exception independently. A completed local test does not approve a real import, prove external compatibility or establish production recovery. Keep manifests, source streams, journals and credentials outside source control.
