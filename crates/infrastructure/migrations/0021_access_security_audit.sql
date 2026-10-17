CREATE TABLE litecr.access_security_audit (
    event_id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    occurred_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    event_type TEXT NOT NULL CHECK (event_type IN (
        'oidc_callback_denied',
        'session_authentication_denied',
        'protected_route_denied'
    )),
    reason_code TEXT NOT NULL CHECK (reason_code IN (
        'invalid_callback',
        'invalid_login_flow',
        'identity_provider_rejected',
        'invalid_identity',
        'identity_not_provisioned',
        'missing_credential',
        'invalid_credential',
        'conflicting_credentials',
        'csrf_rejected',
        'permission_denied'
    )),
    route_family TEXT NOT NULL CHECK (route_family IN (
        'oidc_callback',
        'session',
        'logout',
        'registry',
        'matching',
        'review',
        'migration',
        'configuration',
        'fhir_patient',
        'fhir_metadata',
        'other_protected'
    )),
    scope_shape TEXT NOT NULL CHECK (scope_shape IN (
        'none', 'request_bound', 'path_record', 'unknown'
    )),
    actor_kind TEXT CHECK (actor_kind IS NULL OR actor_kind IN ('human', 'integration_client', 'system')),
    actor_id TEXT,
    request_id TEXT NOT NULL CHECK (request_id ~ '^[!-~]{1,128}$'),
    CHECK ((actor_kind IS NULL) = (actor_id IS NULL)),
    CHECK (actor_id IS NULL OR length(actor_id) BETWEEN 1 AND 512)
);

CREATE INDEX access_security_audit_time_idx
    ON litecr.access_security_audit (occurred_at DESC);
CREATE INDEX access_security_audit_request_idx
    ON litecr.access_security_audit (request_id);

CREATE FUNCTION litecr.prevent_access_security_audit_mutation() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'security audit events are append-only';
END $$;

CREATE TRIGGER access_security_audit_immutable
    BEFORE UPDATE OR DELETE ON litecr.access_security_audit
    FOR EACH ROW EXECUTE FUNCTION litecr.prevent_access_security_audit_mutation();

REVOKE UPDATE, DELETE, TRUNCATE ON litecr.access_security_audit FROM PUBLIC;

DO $$
DECLARE updated_rows INTEGER;
BEGIN
    UPDATE litecr.schema_meta SET version = 21, applied_at = now()
    WHERE singleton = TRUE AND version = 20;
    GET DIAGNOSTICS updated_rows = ROW_COUNT;
    IF updated_rows <> 1 THEN
        RAISE EXCEPTION 'liteCR schema version must be 20 before access security audit migration';
    END IF;
END $$;
