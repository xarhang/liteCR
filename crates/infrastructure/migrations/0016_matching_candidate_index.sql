-- Candidate retrieval keys are materialized with the matcher's Rust normalizer.
-- Replace the wide text B-tree with a fixed-size hash key; the query also compares
-- the full text key, so hash collisions can only add work, never lose a candidate.
DROP INDEX matching.normalized_assertion_exact_scoped_idx;
ALTER TABLE matching.normalized_assertion
    DROP CONSTRAINT normalized_assertion_normalized_key_check,
    ADD CONSTRAINT normalized_assertion_normalized_key_check
        CHECK (length(normalized_key) BETWEEN 1 AND 16384);
CREATE INDEX normalized_assertion_exact_scoped_idx
    ON matching.normalized_assertion
        (registry_id, organization_id, normalizer_version, field_path, md5(normalized_key));
CREATE INDEX normalized_assertion_scoped_key_idx
    ON matching.normalized_assertion
        (registry_id, organization_id, normalizer_version, md5(normalized_key));

CREATE TABLE matching.normalized_identifier (
    source_record_id UUID NOT NULL,
    registry_id TEXT NOT NULL,
    organization_id TEXT NOT NULL,
    receive_version BIGINT NOT NULL,
    authority TEXT NOT NULL,
    identifier_type TEXT NOT NULL,
    normalizer_version TEXT NOT NULL,
    normalized_key TEXT NOT NULL CHECK (length(normalized_key) BETWEEN 1 AND 16384),
    PRIMARY KEY (source_record_id, receive_version, authority, identifier_type, normalizer_version, normalized_key),
    FOREIGN KEY (source_record_id, registry_id, organization_id)
        REFERENCES litecr.source_record (source_record_id, registry_id, organization_id),
    FOREIGN KEY (source_record_id, receive_version)
        REFERENCES litecr.source_record_version (source_record_id, receive_version)
);
CREATE INDEX normalized_identifier_exact_scoped_idx
    ON matching.normalized_identifier
        (registry_id, organization_id, authority, identifier_type, normalizer_version, md5(normalized_key));

-- The marker distinguishes an indexed version with no usable keys from a legacy
-- version that must remain in exhaustive fallback until the bounded CLI reindexes it.
CREATE TABLE matching.indexed_source_version (
    source_record_id UUID NOT NULL,
    receive_version BIGINT NOT NULL,
    normalizer_version TEXT NOT NULL,
    indexed_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (source_record_id, receive_version, normalizer_version),
    FOREIGN KEY (source_record_id, receive_version)
        REFERENCES litecr.source_record_version (source_record_id, receive_version)
);
CREATE INDEX indexed_source_version_version_idx
    ON matching.indexed_source_version (normalizer_version, source_record_id, receive_version);

DO $$
DECLARE updated_rows INTEGER;
BEGIN
    UPDATE litecr.schema_meta SET version = 16, applied_at = now()
    WHERE singleton = TRUE AND version = 15;
    GET DIAGNOSTICS updated_rows = ROW_COUNT;
    IF updated_rows <> 1 THEN
        RAISE EXCEPTION 'liteCR schema version must be 15 before applying matching candidate index migration';
    END IF;
END $$;
