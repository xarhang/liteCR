CREATE TABLE litecr.access_identity (
    actor_kind TEXT NOT NULL CHECK (actor_kind IN ('human', 'integration_client', 'system')),
    issuer_url TEXT NOT NULL DEFAULT '',
    external_subject TEXT NOT NULL,
    is_active BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (actor_kind, issuer_url, external_subject),
    CHECK (length(issuer_url) <= 2048),
    CHECK (length(external_subject) BETWEEN 1 AND 512),
    CHECK (actor_kind <> 'human' OR issuer_url <> '')
);

CREATE TABLE litecr.access_grant (
    grant_id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    actor_kind TEXT NOT NULL,
    issuer_url TEXT NOT NULL DEFAULT '',
    external_subject TEXT NOT NULL,
    registry_id TEXT NOT NULL CHECK (length(registry_id) BETWEEN 1 AND 256),
    organization_id TEXT NOT NULL CHECK (length(organization_id) BETWEEN 1 AND 256),
    record_ids TEXT[],
    role_name TEXT,
    permission_name TEXT,
    grant_source TEXT NOT NULL DEFAULT 'operator' CHECK (grant_source IN ('operator', 'identity_provider', 'bootstrap')),
    granted_by TEXT,
    granted_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    revoked_at TIMESTAMPTZ,
    FOREIGN KEY (actor_kind, issuer_url, external_subject)
        REFERENCES litecr.access_identity (actor_kind, issuer_url, external_subject),
    CHECK (
        (role_name IS NOT NULL AND permission_name IS NULL)
        OR (role_name IS NULL AND permission_name IS NOT NULL)
    ),
    CHECK (role_name IS NULL OR role_name IN (
        'registrar', 'reviewer', 'translator', 'translation_reviewer',
        'administrator', 'integration_client', 'auditor'
    )),
    CHECK (permission_name IS NULL OR permission_name IN (
        'source:write', 'person:read', 'person:search', 'match:request',
        'match:evidence:read', 'receipt:read', 'review:read', 'review:decide',
        'identity:link', 'identity:merge', 'identity:reverse', 'export:read',
        'translation:draft:read', 'translation:draft:write', 'translation:review',
        'translation:publish', 'access:grant:manage', 'configuration:manage',
        'country-profile:manage', 'appearance:manage', 'audit:read'
    )),
    CHECK (role_name IS NULL OR actor_kind <> 'integration_client' OR role_name = 'integration_client'),
    CHECK (permission_name IS NULL OR actor_kind <> 'human' OR permission_name NOT LIKE 'integration:%'),
    CHECK (record_ids IS NULL OR cardinality(record_ids) > 0)
);

CREATE INDEX access_grant_active_actor_scope_idx
    ON litecr.access_grant (actor_kind, issuer_url, external_subject, registry_id, organization_id)
    WHERE revoked_at IS NULL;

CREATE TABLE litecr.access_session (
    session_id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    token_hash BYTEA NOT NULL UNIQUE CHECK (octet_length(token_hash) = 32),
    csrf_hash BYTEA NOT NULL CHECK (octet_length(csrf_hash) = 32),
    actor_kind TEXT NOT NULL,
    issuer_url TEXT NOT NULL DEFAULT '',
    external_subject TEXT NOT NULL,
    oidc_session_id TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at TIMESTAMPTZ NOT NULL,
    last_seen_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    revoked_at TIMESTAMPTZ,
    FOREIGN KEY (actor_kind, issuer_url, external_subject)
        REFERENCES litecr.access_identity (actor_kind, issuer_url, external_subject),
    CHECK (expires_at > created_at)
);

CREATE INDEX access_session_actor_active_idx
    ON litecr.access_session (actor_kind, issuer_url, external_subject, expires_at)
    WHERE revoked_at IS NULL;

CREATE TABLE litecr.access_integration_credential (
    credential_id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    token_hash BYTEA NOT NULL UNIQUE CHECK (octet_length(token_hash) = 32),
    actor_kind TEXT NOT NULL DEFAULT 'integration_client' CHECK (actor_kind = 'integration_client'),
    issuer_url TEXT NOT NULL DEFAULT '',
    external_subject TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at TIMESTAMPTZ,
    last_used_at TIMESTAMPTZ,
    revoked_at TIMESTAMPTZ,
    FOREIGN KEY (actor_kind, issuer_url, external_subject)
        REFERENCES litecr.access_identity (actor_kind, issuer_url, external_subject)
);

CREATE INDEX access_integration_credential_actor_idx
    ON litecr.access_integration_credential (issuer_url, external_subject)
    WHERE revoked_at IS NULL;

CREATE TABLE litecr.oidc_login_transaction (
    transaction_hash BYTEA PRIMARY KEY CHECK (octet_length(transaction_hash) = 32),
    state_hash BYTEA NOT NULL CHECK (octet_length(state_hash) = 32),
    nonce TEXT CHECK (nonce IS NULL OR length(nonce) BETWEEN 32 AND 256),
    pkce_verifier TEXT CHECK (pkce_verifier IS NULL OR length(pkce_verifier) BETWEEN 43 AND 128),
    redirect_uri TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at TIMESTAMPTZ NOT NULL,
    consumed_at TIMESTAMPTZ,
    CHECK (expires_at > created_at),
    CHECK ((consumed_at IS NULL AND nonce IS NOT NULL AND pkce_verifier IS NOT NULL)
        OR (consumed_at IS NOT NULL AND nonce IS NULL AND pkce_verifier IS NULL))
);

CREATE INDEX oidc_login_transaction_expiry_idx
    ON litecr.oidc_login_transaction (expires_at)
    WHERE consumed_at IS NULL;

CREATE TABLE litecr.access_audit (
    audit_id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    occurred_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    actor_kind TEXT NOT NULL CHECK (actor_kind IN ('human', 'integration_client', 'system')),
    actor_issuer_url TEXT NOT NULL DEFAULT '',
    actor_id TEXT NOT NULL,
    registry_id TEXT,
    organization_id TEXT,
    action TEXT NOT NULL CHECK (length(action) BETWEEN 1 AND 128),
    resource_type TEXT,
    resource_ref TEXT,
    outcome TEXT NOT NULL CHECK (outcome IN ('allowed', 'denied', 'revoked', 'failed')),
    reason_code TEXT,
    request_id TEXT NOT NULL,
    correlation_id TEXT,
    policy_version TEXT NOT NULL DEFAULT 'access-v1',
    CHECK (length(actor_issuer_url) <= 2048),
    CHECK (resource_ref IS NULL OR length(resource_ref) <= 512)
);

CREATE INDEX access_audit_time_idx ON litecr.access_audit (occurred_at DESC);
CREATE INDEX access_audit_actor_time_idx ON litecr.access_audit (actor_kind, actor_issuer_url, actor_id, occurred_at DESC);

DO $$
DECLARE
    updated_rows INTEGER;
BEGIN
    UPDATE litecr.schema_meta
    SET version = 2, applied_at = now()
    WHERE singleton = TRUE AND version = 1;
    GET DIAGNOSTICS updated_rows = ROW_COUNT;
    IF updated_rows <> 1 THEN
        RAISE EXCEPTION 'liteCR schema version must be 1 before applying access migration';
    END IF;
END $$;
