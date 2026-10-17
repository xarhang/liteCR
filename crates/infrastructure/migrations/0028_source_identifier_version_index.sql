-- Match/FHIR projections read all live identifiers for one accepted source version.
-- Non-unique: shared or repeated non-authoritative identifiers are not identities.
CREATE INDEX source_identifier_assertion_live_source_version_idx
    ON litecr.source_identifier_assertion (source_record_id, receive_version)
    WHERE withdrawn_at IS NULL;

DO $$
DECLARE updated_rows INTEGER;
BEGIN
    UPDATE litecr.schema_meta SET version=28, applied_at=now()
    WHERE singleton=TRUE AND version=27;
    GET DIAGNOSTICS updated_rows=ROW_COUNT;
    IF updated_rows<>1 THEN
        RAISE EXCEPTION 'liteCR schema version must be 27 before applying source identifier version index';
    END IF;
END $$;
