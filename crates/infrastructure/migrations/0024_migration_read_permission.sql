-- Allow migration operators to inspect scoped checkpoints and reverse
-- eligibility without granting package validation, staging, apply, disposition,
-- or reverse operations. Migration writers remain readers for CLI resumability.
ALTER TABLE litecr.access_grant
    DROP CONSTRAINT access_grant_permission_name_check;

ALTER TABLE litecr.access_grant
    ADD CONSTRAINT access_grant_permission_name_check CHECK (
        permission_name IS NULL OR permission_name IN (
            'source:write', 'migration:read', 'migration:write', 'person:read', 'person:search', 'match:request',
            'match:evidence:read', 'receipt:read', 'review:read', 'review:decide',
            'identity:link', 'identity:merge', 'identity:reverse', 'export:read',
            'translation:draft:read', 'translation:draft:write', 'translation:review',
            'translation:publish', 'access:grant:manage', 'configuration:manage',
            'country-profile:manage', 'appearance:manage', 'audit:read'
        )
    );

DO $$
DECLARE updated_rows INTEGER;
BEGIN
    UPDATE litecr.schema_meta
    SET version = 24, applied_at = now()
    WHERE singleton = TRUE AND version = 23;
    GET DIAGNOSTICS updated_rows = ROW_COUNT;
    IF updated_rows <> 1 THEN
        RAISE EXCEPTION 'liteCR schema version must be 23 before migration read permission migration';
    END IF;
END $$;
