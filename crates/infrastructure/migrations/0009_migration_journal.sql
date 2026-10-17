CREATE TABLE litecr.migration_checkpoint (
    manifest_sha256 BYTEA PRIMARY KEY CHECK (octet_length(manifest_sha256) = 32),
    stream_sha256 BYTEA NOT NULL CHECK (octet_length(stream_sha256) = 32),
    snapshot_id TEXT NOT NULL CHECK (length(snapshot_id) BETWEEN 1 AND 512),
    source_system_id TEXT NOT NULL CHECK (length(source_system_id) BETWEEN 1 AND 256),
    registry_id TEXT NOT NULL CHECK (length(registry_id) BETWEEN 1 AND 256),
    organization_id TEXT NOT NULL CHECK (length(organization_id) BETWEEN 1 AND 256),
    expected_record_count BIGINT NOT NULL CHECK (expected_record_count >= 0),
    committed_batch_sequence BIGINT NOT NULL DEFAULT 0 CHECK (committed_batch_sequence >= 0),
    committed_record_count BIGINT NOT NULL DEFAULT 0 CHECK (committed_record_count >= 0),
    state TEXT NOT NULL CHECK (state IN ('validated','importing','completed','reverse_blocked','reversed')),
    created_by TEXT NOT NULL CHECK (length(created_by) BETWEEN 1 AND 512),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (manifest_sha256, registry_id, organization_id)
);

CREATE TABLE litecr.migration_batch (
    manifest_sha256 BYTEA NOT NULL REFERENCES litecr.migration_checkpoint(manifest_sha256),
    batch_sequence BIGINT NOT NULL CHECK (batch_sequence > 0),
    stream_sha256 BYTEA NOT NULL CHECK (octet_length(stream_sha256) = 32),
    batch_sha256 BYTEA NOT NULL CHECK (octet_length(batch_sha256) = 32),
    record_count INTEGER NOT NULL CHECK (record_count BETWEEN 1 AND 500),
    first_input_line BIGINT NOT NULL CHECK (first_input_line > 0),
    last_input_line BIGINT NOT NULL CHECK (last_input_line >= first_input_line),
    committed_by TEXT NOT NULL CHECK (length(committed_by) BETWEEN 1 AND 512),
    committed_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (manifest_sha256, batch_sequence),
    CHECK (octet_length(manifest_sha256) = 32)
);

CREATE TABLE litecr.migration_journal (
    journal_id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    manifest_sha256 BYTEA NOT NULL,
    batch_sequence BIGINT NOT NULL,
    source_system_id TEXT NOT NULL CHECK (length(source_system_id) BETWEEN 1 AND 256),
    source_record_key TEXT NOT NULL CHECK (length(source_record_key) BETWEEN 1 AND 512),
    source_version TEXT NOT NULL CHECK (length(source_version) BETWEEN 1 AND 512),
    source_payload_sha256 BYTEA NOT NULL CHECK (octet_length(source_payload_sha256) = 32),
    original_payload_ndjson BYTEA NOT NULL CHECK (octet_length(original_payload_ndjson) BETWEEN 2 AND 1048576),
    preserved_public_id TEXT CHECK (preserved_public_id IS NULL OR length(preserved_public_id) BETWEEN 1 AND 512),
    preserved_cruid TEXT CHECK (preserved_cruid IS NULL OR length(preserved_cruid) BETWEEN 1 AND 512),
    imported_public_id TEXT NOT NULL CHECK (length(imported_public_id) BETWEEN 1 AND 512),
    imported_source_record_id UUID NOT NULL REFERENCES litecr.source_record(source_record_id),
    imported_receive_version BIGINT NOT NULL CHECK (imported_receive_version > 0),
    expected_current_payload_sha256 BYTEA NOT NULL CHECK (octet_length(expected_current_payload_sha256) = 32),
    expected_current_receive_version BIGINT NOT NULL CHECK (expected_current_receive_version > 0),
    reverse_state TEXT NOT NULL DEFAULT 'pending' CHECK (reverse_state IN ('pending','reversed','refused')),
    reverse_refusal_code TEXT CHECK (reverse_refusal_code IS NULL OR length(reverse_refusal_code) <= 128),
    imported_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    reversed_at TIMESTAMPTZ,
    FOREIGN KEY (manifest_sha256, batch_sequence)
        REFERENCES litecr.migration_batch(manifest_sha256, batch_sequence),
    UNIQUE (manifest_sha256, source_system_id, source_record_key, source_version),
    CHECK (octet_length(manifest_sha256) = 32)
);

CREATE INDEX migration_journal_source_key_idx
    ON litecr.migration_journal (source_system_id, source_record_key, imported_at DESC);

DO $$
DECLARE updated_rows INTEGER;
BEGIN
    UPDATE litecr.schema_meta SET version = 9, applied_at = now()
    WHERE singleton = TRUE AND version = 8;
    GET DIAGNOSTICS updated_rows = ROW_COUNT;
    IF updated_rows <> 1 THEN
        RAISE EXCEPTION 'liteCR schema version must be 8 before applying migration journal migration';
    END IF;
END $$;
