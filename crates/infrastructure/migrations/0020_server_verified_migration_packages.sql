CREATE TABLE litecr.migration_package_v2 (
    upload_id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    manifest_sha256 BYTEA NOT NULL UNIQUE CHECK (octet_length(manifest_sha256) = 32),
    stream_sha256 BYTEA NOT NULL CHECK (octet_length(stream_sha256) = 32),
    snapshot_id TEXT NOT NULL CHECK (length(snapshot_id) BETWEEN 1 AND 512),
    source_system_id TEXT NOT NULL CHECK (length(source_system_id) BETWEEN 1 AND 256),
    registry_id TEXT NOT NULL CHECK (length(registry_id) BETWEEN 1 AND 256),
    organization_id TEXT NOT NULL CHECK (length(organization_id) BETWEEN 1 AND 256),
    expected_record_count BIGINT NOT NULL CHECK (expected_record_count >= 0),
    expected_stream_bytes BIGINT NOT NULL CHECK (expected_stream_bytes BETWEEN 0 AND 21474836480),
    state TEXT NOT NULL CHECK (state IN ('receiving','verifying','ready','applying','completed','rejected','expired')),
    staged_batch_count BIGINT NOT NULL DEFAULT 0 CHECK (staged_batch_count >= 0),
    staged_bytes BIGINT NOT NULL DEFAULT 0 CHECK (staged_bytes >= 0),
    reserved_bytes BIGINT NOT NULL DEFAULT 0 CHECK (reserved_bytes >= 0),
    verified_stream_bytes BIGINT NOT NULL DEFAULT 0 CHECK (verified_stream_bytes >= 0),
    staged_record_count BIGINT NOT NULL DEFAULT 0 CHECK (staged_record_count >= 0),
    committed_batch_sequence BIGINT NOT NULL DEFAULT 0 CHECK (committed_batch_sequence >= 0),
    committed_record_count BIGINT NOT NULL DEFAULT 0 CHECK (committed_record_count >= 0),
    storage_protection_reference TEXT NOT NULL CHECK (length(storage_protection_reference) BETWEEN 1 AND 512),
    created_by TEXT NOT NULL CHECK (length(created_by) BETWEEN 1 AND 512),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at TIMESTAMPTZ NOT NULL,
    CHECK (committed_batch_sequence <= staged_batch_count),
    CHECK (staged_record_count <= expected_record_count),
    CHECK (staged_bytes <= expected_stream_bytes),
    CHECK (reserved_bytes <= expected_stream_bytes)
);

CREATE TABLE litecr.migration_package_v2_batch (
    upload_id UUID NOT NULL REFERENCES litecr.migration_package_v2(upload_id),
    batch_sequence BIGINT NOT NULL CHECK (batch_sequence > 0),
    ndjson_bytes BYTEA NOT NULL CHECK (octet_length(ndjson_bytes) BETWEEN 1 AND 8388608),
    batch_sha256 BYTEA NOT NULL CHECK (octet_length(batch_sha256) = 32),
    first_input_line BIGINT NOT NULL CHECK (first_input_line > 0),
    last_input_line BIGINT NOT NULL CHECK (last_input_line >= first_input_line),
    record_count INTEGER NOT NULL CHECK (record_count BETWEEN 1 AND 500),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (upload_id, batch_sequence)
);

CREATE INDEX migration_package_v2_expiry_idx
    ON litecr.migration_package_v2 (expires_at)
    WHERE state IN ('receiving','verifying','ready');

DO $$
DECLARE updated_rows INTEGER;
BEGIN
    UPDATE litecr.schema_meta SET version = 20, applied_at = now()
    WHERE singleton = TRUE AND version = 19;
    GET DIAGNOSTICS updated_rows = ROW_COUNT;
    IF updated_rows <> 1 THEN
        RAISE EXCEPTION 'liteCR schema version must be 19 before server-verified migration package migration';
    END IF;
END $$;
