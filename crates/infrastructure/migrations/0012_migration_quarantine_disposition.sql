CREATE TABLE litecr.migration_quarantine_disposition (
    disposition_id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    action TEXT NOT NULL CHECK (action IN ('corrected_resubmission','deferred','duplicate_acknowledged')),
    parent_manifest_sha256 BYTEA NOT NULL CHECK (octet_length(parent_manifest_sha256) = 32),
    replacement_manifest_sha256 BYTEA CHECK (replacement_manifest_sha256 IS NULL OR octet_length(replacement_manifest_sha256) = 32),
    registry_id TEXT NOT NULL CHECK (length(registry_id) BETWEEN 1 AND 256),
    organization_id TEXT NOT NULL CHECK (length(organization_id) BETWEEN 1 AND 256),
    actor_kind TEXT NOT NULL CHECK (actor_kind IN ('human','integration_client','system')),
    actor_issuer_url TEXT NOT NULL DEFAULT '',
    actor_id TEXT NOT NULL CHECK (length(actor_id) BETWEEN 1 AND 512),
    reason TEXT NOT NULL CHECK (length(reason) BETWEEN 1 AND 2000),
    idempotency_key_hash BYTEA NOT NULL CHECK (octet_length(idempotency_key_hash) = 32),
    request_fingerprint BYTEA NOT NULL CHECK (octet_length(request_fingerprint) = 32),
    correction_count INTEGER NOT NULL CHECK (correction_count BETWEEN 1 AND 1000),
    request_id TEXT NOT NULL,
    correlation_id TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK ((action = 'deferred') = (replacement_manifest_sha256 IS NULL)),
    CHECK (replacement_manifest_sha256 IS NULL OR replacement_manifest_sha256 <> parent_manifest_sha256),
    UNIQUE (actor_kind, actor_issuer_url, actor_id, registry_id, organization_id, idempotency_key_hash)
);

CREATE TABLE litecr.migration_quarantine_disposition_item (
    disposition_id UUID NOT NULL REFERENCES litecr.migration_quarantine_disposition(disposition_id),
    input_line BIGINT NOT NULL CHECK (input_line > 0),
    duplicate_of_line BIGINT CHECK (duplicate_of_line IS NULL OR (duplicate_of_line > 0 AND duplicate_of_line < input_line)),
    quarantine_code TEXT NOT NULL CHECK (length(quarantine_code) BETWEEN 1 AND 64),
    original_payload_sha256 BYTEA NOT NULL CHECK (octet_length(original_payload_sha256) = 32),
    corrected_payload_sha256 BYTEA CHECK (corrected_payload_sha256 IS NULL OR octet_length(corrected_payload_sha256) = 32),
    PRIMARY KEY (disposition_id, input_line),
    CHECK ((quarantine_code = 'EXACT_REPLAY_DUPLICATE') = (duplicate_of_line IS NOT NULL)),
    CHECK (quarantine_code <> 'EXACT_REPLAY_DUPLICATE' OR
           (corrected_payload_sha256 IS NOT NULL AND corrected_payload_sha256 = original_payload_sha256))
);

CREATE INDEX migration_quarantine_disposition_parent_idx
    ON litecr.migration_quarantine_disposition (parent_manifest_sha256, created_at DESC);

DO $$
DECLARE updated_rows INTEGER;
BEGIN
    UPDATE litecr.schema_meta SET version = 12, applied_at = now()
    WHERE singleton = TRUE AND version = 11;
    GET DIAGNOSTICS updated_rows = ROW_COUNT;
    IF updated_rows <> 1 THEN
        RAISE EXCEPTION 'liteCR schema version must be 11 before applying migration quarantine disposition migration';
    END IF;
END $$;
