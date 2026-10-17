-- External subject identifiers are only unique within their identity issuer.
-- Existing assignments cannot be safely backfilled because the old schema did
-- not retain the issuer; keep them unbound so only record grants can reveal
-- those cases until an authorized reviewer reassigns them.
ALTER TABLE litecr.review_case
    ADD COLUMN assigned_actor_issuer_url TEXT
        CHECK (assigned_actor_issuer_url IS NULL OR
               (assigned_actor_id IS NOT NULL AND length(assigned_actor_issuer_url) BETWEEN 1 AND 2048));

DO $$
DECLARE updated_rows INTEGER;
BEGIN
    UPDATE litecr.schema_meta SET version = 23, applied_at = now()
    WHERE singleton = TRUE AND version = 22;
    GET DIAGNOSTICS updated_rows = ROW_COUNT;
    IF updated_rows <> 1 THEN
        RAISE EXCEPTION 'liteCR schema version must be 22 before review assignment issuer migration';
    END IF;
END $$;
