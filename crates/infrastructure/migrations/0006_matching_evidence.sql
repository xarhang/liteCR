CREATE SCHEMA matching;

CREATE TABLE matching.normalized_assertion (
    source_record_id UUID NOT NULL,
    registry_id TEXT NOT NULL,
    organization_id TEXT NOT NULL,
    receive_version BIGINT NOT NULL,
    field_path TEXT NOT NULL,
    normalizer_version TEXT NOT NULL,
    normalized_key TEXT NOT NULL CHECK (length(normalized_key) BETWEEN 1 AND 4096),
    representation TEXT NOT NULL CHECK (representation IN ('source_original', 'source_variant', 'transliteration')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (source_record_id, receive_version, field_path, normalizer_version, normalized_key),
    FOREIGN KEY (source_record_id, registry_id, organization_id)
        REFERENCES litecr.source_record (source_record_id, registry_id, organization_id),
    FOREIGN KEY (source_record_id, receive_version, field_path)
        REFERENCES litecr.source_assertion (source_record_id, receive_version, field_path)
);

CREATE INDEX normalized_assertion_exact_scoped_idx
    ON matching.normalized_assertion (registry_id, organization_id, normalizer_version, field_path, normalized_key);
CREATE INDEX normalized_assertion_trgm_scoped_idx
    ON matching.normalized_assertion USING GIN (normalized_key gin_trgm_ops);

CREATE TABLE matching.evidence_run (
    run_id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    registry_id TEXT NOT NULL,
    organization_id TEXT NOT NULL,
    query_source_record_id UUID NOT NULL,
    query_receive_version BIGINT NOT NULL,
    query_source_version TEXT,
    engine_version TEXT NOT NULL,
    normalization_version TEXT NOT NULL,
    candidate_version TEXT NOT NULL,
    feature_version TEXT NOT NULL,
    policy_version TEXT NOT NULL,
    source_snapshot_digest BYTEA NOT NULL CHECK (octet_length(source_snapshot_digest) = 32),
    candidate_count INTEGER NOT NULL CHECK (candidate_count BETWEEN 0 AND 200),
    candidates_truncated BOOLEAN NOT NULL,
    actor_id TEXT NOT NULL CHECK (length(actor_id) BETWEEN 1 AND 512),
    request_id TEXT NOT NULL CHECK (length(request_id) BETWEEN 1 AND 256),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    FOREIGN KEY (query_source_record_id, registry_id, organization_id)
        REFERENCES litecr.source_record (source_record_id, registry_id, organization_id)
);

CREATE TABLE matching.evidence_candidate (
    run_id UUID NOT NULL REFERENCES matching.evidence_run(run_id),
    candidate_source_record_id UUID NOT NULL,
    candidate_receive_version BIGINT NOT NULL,
    candidate_source_version TEXT,
    score_points SMALLINT NOT NULL,
    score_type TEXT NOT NULL CHECK (score_type = 'raw_heuristic_points_not_probability'),
    advisory_outcome TEXT NOT NULL CHECK (advisory_outcome IN ('review', 'insufficient_evidence', 'conflict_review')),
    evidence JSONB NOT NULL CHECK (octet_length(evidence::text) <= 16384),
    PRIMARY KEY (run_id, candidate_source_record_id),
    FOREIGN KEY (candidate_source_record_id, candidate_receive_version)
        REFERENCES litecr.source_record_version(source_record_id, receive_version)
);

CREATE INDEX evidence_run_scope_time_idx
    ON matching.evidence_run (registry_id, organization_id, created_at DESC);

DO $$
DECLARE updated_rows INTEGER;
BEGIN
    UPDATE litecr.schema_meta SET version = 6, applied_at = now()
    WHERE singleton = TRUE AND version = 5;
    GET DIAGNOSTICS updated_rows = ROW_COUNT;
    IF updated_rows <> 1 THEN
        RAISE EXCEPTION 'liteCR schema version must be 5 before applying matching migration';
    END IF;
END $$;
