-- The worker must recheck and lock source snapshots without receiving UPDATE
-- privileges on registry facts. This fixed SECURITY DEFINER function is owned
-- by litecr_migrator and exposes only scoped, bounded freshness rows.
CREATE FUNCTION matching.lock_advisory_source_snapshot(
    p_registry_id TEXT,
    p_organization_id TEXT,
    p_source_record_ids TEXT[]
)
RETURNS TABLE (
    source_record_id UUID,
    current_receive_version BIGINT,
    version_state TEXT,
    source_active BOOLEAN,
    version_active BOOLEAN
)
LANGUAGE plpgsql
VOLATILE
SECURITY DEFINER
SET search_path = pg_catalog
AS $$
BEGIN
    IF p_registry_id IS NULL OR length(p_registry_id) NOT BETWEEN 1 AND 256
       OR p_organization_id IS NULL OR length(p_organization_id) NOT BETWEEN 1 AND 256
       OR p_source_record_ids IS NULL
       OR cardinality(p_source_record_ids) NOT BETWEEN 1 AND 201
       OR array_position(p_source_record_ids, NULL) IS NOT NULL
       OR cardinality(p_source_record_ids) <> (
            SELECT count(DISTINCT requested.source_record_id)
            FROM unnest(p_source_record_ids) AS requested(source_record_id)
       ) THEN
        RAISE EXCEPTION 'ADVISORY_SOURCE_LOCK_ARGUMENTS_INVALID'
            USING ERRCODE = '22023';
    END IF;
    IF EXISTS (
        SELECT 1 FROM unnest(p_source_record_ids) AS requested(source_record_id)
        WHERE requested.source_record_id !~ '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$'
    ) THEN
        RAISE EXCEPTION 'ADVISORY_SOURCE_LOCK_ID_INVALID'
            USING ERRCODE = '22023';
    END IF;

    RETURN QUERY
    SELECT sr.source_record_id,
           sr.current_receive_version,
           sv.version_state,
           (sr.withdrawn_at IS NULL),
           (sv.withdrawn_at IS NULL)
    FROM litecr.source_record AS sr
    JOIN litecr.source_record_version AS sv
      ON sv.source_record_id = sr.source_record_id
     AND sv.receive_version = sr.current_receive_version
    WHERE sr.registry_id = p_registry_id
      AND sr.organization_id = p_organization_id
      AND sr.source_record_id::text = ANY(p_source_record_ids)
    ORDER BY sr.source_record_id
    FOR UPDATE OF sr, sv;
END
$$;

REVOKE ALL ON FUNCTION matching.lock_advisory_source_snapshot(TEXT, TEXT, TEXT[]) FROM PUBLIC;

DO $$
DECLARE updated_rows INTEGER;
BEGIN
    UPDATE litecr.schema_meta
    SET version = 14, applied_at = now()
    WHERE singleton = TRUE AND version = 13;
    GET DIAGNOSTICS updated_rows = ROW_COUNT;
    IF updated_rows <> 1 THEN
        RAISE EXCEPTION 'liteCR schema version must be 13 before applying advisory source lock migration';
    END IF;
END $$;
