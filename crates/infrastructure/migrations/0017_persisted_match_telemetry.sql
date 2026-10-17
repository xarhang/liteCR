-- Preserve the exact API MatchResult for each immutable evidence run.
-- Older runs retain NULL and are served with explicitly incomplete reconstruction.
ALTER TABLE matching.evidence_run
    ADD COLUMN result_snapshot JSONB
        CHECK (result_snapshot IS NULL OR octet_length(result_snapshot::text) <= 4194304);

DO $$
DECLARE updated_rows INTEGER;
BEGIN
    UPDATE litecr.schema_meta SET version = 17, applied_at = now()
    WHERE singleton = TRUE AND version = 16;
    GET DIAGNOSTICS updated_rows = ROW_COUNT;
    IF updated_rows <> 1 THEN
        RAISE EXCEPTION 'liteCR schema version must be 16 before applying persisted matching telemetry migration';
    END IF;
END $$;
