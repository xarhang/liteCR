ALTER TABLE litecr.migration_effect
    ADD COLUMN batch_sequence BIGINT;

-- Effects created before this migration were recorded at manifest/source
-- granularity. Recover their introducing batch from the exact staged payload
-- that first contains the alias or identifier. Refuse to apply if any effect
-- cannot be attributed unambiguously to a journaled batch.
UPDATE litecr.migration_effect e
SET batch_sequence = (
    SELECT MIN(j.batch_sequence)
    FROM litecr.migration_journal j
    JOIN litecr.person_alias a ON a.alias_id = e.effect_ref
    CROSS JOIN LATERAL (
        SELECT convert_from(j.original_payload_ndjson, 'UTF8')::jsonb AS payload
    ) source_row
    WHERE j.manifest_sha256 = e.manifest_sha256
      AND j.imported_source_record_id = e.source_record_id
      AND e.effect_kind = 'person_alias'
      AND (
          COALESCE(source_row.payload -> 'aliases', '[]'::jsonb) ? a.alias_value
          OR source_row.payload ->> 'public_id' = a.alias_value
          OR source_row.payload ->> 'cruid' = a.alias_value
      )
)
WHERE e.effect_kind = 'person_alias';

UPDATE litecr.migration_effect e
SET batch_sequence = (
    SELECT MIN(j.batch_sequence)
    FROM litecr.migration_journal j
    JOIN litecr.source_identifier_assertion i
      ON i.source_record_id = j.imported_source_record_id
     AND i.receive_version = j.imported_receive_version
    CROSS JOIN LATERAL (
        SELECT convert_from(j.original_payload_ndjson, 'UTF8')::jsonb AS payload
    ) source_row
    WHERE j.manifest_sha256 = e.manifest_sha256
      AND j.imported_source_record_id = e.source_record_id
      AND e.effect_kind = 'person_identifier'
      AND i.authoritative
      AND i.assertion_state = 'accepted'
      AND EXISTS (
          SELECT 1 FROM litecr.source_record_version v
          WHERE v.source_record_id = i.source_record_id
            AND v.receive_version = i.receive_version
            AND v.version_state = 'accepted'
      )
      AND EXISTS (
          SELECT 1
          FROM jsonb_array_elements(
              COALESCE(source_row.payload -> 'identifiers', '[]'::jsonb)
          ) identifier
          WHERE identifier ->> 'authority' = i.authority
            AND identifier ->> 'type' = i.identifier_type
            AND identifier ->> 'value' = i.original_value
      )
)
WHERE e.effect_kind = 'person_identifier';

DO $$
BEGIN
    IF EXISTS (
        SELECT 1 FROM litecr.migration_effect WHERE batch_sequence IS NULL
    ) THEN
        RAISE EXCEPTION 'cannot attribute every migration effect to its introducing batch';
    END IF;
END $$;

ALTER TABLE litecr.migration_effect
    ALTER COLUMN batch_sequence SET NOT NULL,
    ADD CONSTRAINT migration_effect_batch_fk
        FOREIGN KEY (manifest_sha256, batch_sequence)
        REFERENCES litecr.migration_batch (manifest_sha256, batch_sequence);

CREATE INDEX migration_effect_batch_source_idx
    ON litecr.migration_effect (manifest_sha256, batch_sequence, source_record_id, effect_kind);

DO $$
DECLARE updated_rows INTEGER;
BEGIN
    UPDATE litecr.schema_meta SET version = 11, applied_at = now()
    WHERE singleton = TRUE AND version = 10;
    GET DIAGNOSTICS updated_rows = ROW_COUNT;
    IF updated_rows <> 1 THEN
        RAISE EXCEPTION 'liteCR schema version must be 10 before migration effect batch migration';
    END IF;
END $$;
