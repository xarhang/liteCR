-- Bind a review case to the exact immutable advisory evidence snapshot when
-- one is supplied. NULL remains valid for legacy/manual cases.
ALTER TABLE matching.evidence_run
    ADD CONSTRAINT evidence_run_case_query_key
    UNIQUE (run_id, registry_id, organization_id, query_source_record_id, query_receive_version);

ALTER TABLE matching.evidence_candidate
    ADD CONSTRAINT evidence_candidate_case_key
    UNIQUE (run_id, candidate_source_record_id, candidate_receive_version);

ALTER TABLE litecr.review_case
    ADD COLUMN evidence_run_id UUID;

ALTER TABLE litecr.review_case
    ADD CONSTRAINT review_case_evidence_query_fk
        FOREIGN KEY (evidence_run_id, registry_id, organization_id,
                     query_source_record_id, query_receive_version)
        REFERENCES matching.evidence_run
            (run_id, registry_id, organization_id, query_source_record_id, query_receive_version),
    ADD CONSTRAINT review_case_evidence_candidate_fk
        FOREIGN KEY (evidence_run_id, candidate_source_record_id, candidate_receive_version)
        REFERENCES matching.evidence_candidate
            (run_id, candidate_source_record_id, candidate_receive_version);

CREATE INDEX review_case_evidence_run_idx
    ON litecr.review_case (evidence_run_id)
    WHERE evidence_run_id IS NOT NULL;

DO $$
DECLARE updated_rows INTEGER;
BEGIN
    UPDATE litecr.schema_meta SET version = 15, applied_at = now()
    WHERE singleton = TRUE AND version = 14;
    GET DIAGNOSTICS updated_rows = ROW_COUNT;
    IF updated_rows <> 1 THEN
        RAISE EXCEPTION 'liteCR schema version must be 14 before applying review evidence provenance migration';
    END IF;
END $$;
