-- Optional API idempotency for durable match evidence. Only digests are stored;
-- the original evidence run remains the single source of the replayed response.
CREATE TABLE matching.request_idempotency (
    actor_digest BYTEA NOT NULL CHECK (octet_length(actor_digest) = 32),
    registry_id TEXT NOT NULL,
    organization_id TEXT NOT NULL,
    query_source_record_id UUID NOT NULL,
    key_digest BYTEA NOT NULL CHECK (octet_length(key_digest) = 32),
    request_digest BYTEA NOT NULL CHECK (octet_length(request_digest) = 32),
    evidence_run_id UUID NOT NULL UNIQUE
        REFERENCES matching.evidence_run(run_id) ON DELETE RESTRICT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (
        actor_digest,
        registry_id,
        organization_id,
        query_source_record_id,
        key_digest
    ),
    FOREIGN KEY (query_source_record_id, registry_id, organization_id)
        REFERENCES litecr.source_record (source_record_id, registry_id, organization_id)
);

CREATE INDEX request_idempotency_run_idx
    ON matching.request_idempotency (evidence_run_id);

DO $$
DECLARE updated_rows INTEGER;
BEGIN
    UPDATE litecr.schema_meta SET version = 26, applied_at = now()
    WHERE singleton = TRUE AND version = 25;
    GET DIAGNOSTICS updated_rows = ROW_COUNT;
    IF updated_rows <> 1 THEN
        RAISE EXCEPTION 'liteCR schema version must be 25 before applying matching idempotency migration';
    END IF;
END $$;
