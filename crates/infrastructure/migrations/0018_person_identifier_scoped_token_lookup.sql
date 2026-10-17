CREATE INDEX person_identifier_scoped_token_lookup_idx
    ON litecr.person_identifier (registry_id, organization_id, identifier_value, authority);

DO $$
DECLARE updated_rows INTEGER;
BEGIN
    UPDATE litecr.schema_meta SET version = 18, applied_at = now()
    WHERE singleton = TRUE AND version = 17;
    GET DIAGNOSTICS updated_rows = ROW_COUNT;
    IF updated_rows <> 1 THEN
        RAISE EXCEPTION 'liteCR schema version must be 17 before applying scoped identifier lookup index migration';
    END IF;
END $$;
