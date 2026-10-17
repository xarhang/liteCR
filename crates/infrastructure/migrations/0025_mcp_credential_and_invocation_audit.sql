-- Separate MCP stdio credentials from generic IntegrationClient API credentials.
-- No transport, endpoint, or tool is enabled by this schema change.
CREATE TABLE litecr.access_mcp_stdio_credential (
    credential_id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    token_hash BYTEA NOT NULL UNIQUE CHECK (octet_length(token_hash) = 32),
    actor_kind TEXT NOT NULL DEFAULT 'integration_client'
        CHECK (actor_kind = 'integration_client'),
    issuer_url TEXT NOT NULL DEFAULT '',
    external_subject TEXT NOT NULL,
    audience TEXT NOT NULL CHECK (audience = 'urn:litecr:mcp:stdio'),
    client_id TEXT NOT NULL CHECK (client_id ~ '^[A-Za-z0-9][A-Za-z0-9._:-]{0,95}$'),
    purpose_code TEXT NOT NULL CHECK (purpose_code ~ '^[A-Za-z0-9][A-Za-z0-9._:-]{0,95}$'),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at TIMESTAMPTZ NOT NULL,
    last_used_at TIMESTAMPTZ,
    revoked_at TIMESTAMPTZ,
    FOREIGN KEY (actor_kind, issuer_url, external_subject)
        REFERENCES litecr.access_identity (actor_kind, issuer_url, external_subject),
    CHECK (expires_at > created_at AND expires_at <= created_at + interval '15 minutes')
);

CREATE INDEX access_mcp_stdio_credential_actor_active_idx
    ON litecr.access_mcp_stdio_credential (issuer_url, external_subject, expires_at)
    WHERE revoked_at IS NULL;

CREATE TABLE litecr.access_mcp_stdio_credential_tool (
    credential_id BIGINT NOT NULL REFERENCES litecr.access_mcp_stdio_credential (credential_id),
    tool_id TEXT NOT NULL CHECK (tool_id IN (
        'registry.get_record_summary',
        'registry.search_records',
        'matching.request_candidates',
        'matching.get_evidence_summary',
        'review.get_case_summary'
    )),
    PRIMARY KEY (credential_id, tool_id)
);

CREATE TABLE litecr.mcp_invocation_audit (
    event_id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    occurred_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    request_id TEXT NOT NULL CHECK (request_id ~ '^[0-9a-f]{64}$'),
    client_id TEXT CHECK (client_id IS NULL OR client_id ~ '^[A-Za-z0-9][A-Za-z0-9._:-]{0,95}$'),
    purpose_code TEXT CHECK (purpose_code IS NULL OR purpose_code ~ '^[A-Za-z0-9][A-Za-z0-9._:-]{0,95}$'),
    actor_identity_key BIGINT REFERENCES litecr.access_identity (identity_key),
    tool_id TEXT NOT NULL CHECK (tool_id IN (
        'registry.get_record_summary',
        'registry.search_records',
        'matching.request_candidates',
        'matching.get_evidence_summary',
        'review.get_case_summary'
    )),
    registry_id TEXT,
    organization_id TEXT,
    outcome TEXT NOT NULL CHECK (outcome IN ('succeeded', 'denied')),
    reason_code TEXT NOT NULL CHECK (reason_code IN (
        'none', 'unauthenticated', 'invalid_credential', 'tool_not_allowed',
        'access_denied', 'not_found', 'invalid_input', 'unavailable'
    )),
    result_count INTEGER NOT NULL CHECK (result_count BETWEEN 0 AND 100),
    CHECK ((client_id IS NULL) = (purpose_code IS NULL)),
    CHECK ((registry_id IS NULL) = (organization_id IS NULL)),
    CHECK ((outcome = 'succeeded' AND reason_code = 'none' AND actor_identity_key IS NOT NULL
            AND client_id IS NOT NULL AND registry_id IS NOT NULL)
        OR (outcome = 'denied' AND reason_code <> 'none' AND result_count = 0))
);

CREATE INDEX mcp_invocation_audit_time_idx
    ON litecr.mcp_invocation_audit (occurred_at DESC);
CREATE INDEX mcp_invocation_audit_client_tool_idx
    ON litecr.mcp_invocation_audit (client_id, tool_id, occurred_at DESC);

CREATE TRIGGER mcp_invocation_audit_immutable
    BEFORE UPDATE OR DELETE ON litecr.mcp_invocation_audit
    FOR EACH ROW EXECUTE FUNCTION litecr.prevent_access_security_audit_mutation();
REVOKE UPDATE, DELETE, TRUNCATE ON litecr.mcp_invocation_audit FROM PUBLIC;

DO $$
DECLARE updated_rows INTEGER;
BEGIN
    UPDATE litecr.schema_meta SET version = 25, applied_at = now()
    WHERE singleton = TRUE AND version = 24;
    GET DIAGNOSTICS updated_rows = ROW_COUNT;
    IF updated_rows <> 1 THEN
        RAISE EXCEPTION 'liteCR schema version must be 24 before MCP credential/audit migration';
    END IF;
END $$;
