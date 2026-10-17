CREATE TABLE litecr.person (
    person_id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    public_id TEXT NOT NULL UNIQUE,
    registry_id TEXT NOT NULL CHECK (length(registry_id) BETWEEN 1 AND 256),
    organization_id TEXT NOT NULL CHECK (length(organization_id) BETWEEN 1 AND 256),
    resource_version BIGINT NOT NULL DEFAULT 1 CHECK (resource_version > 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (person_id, registry_id, organization_id)
);

CREATE TABLE litecr.person_alias (
    alias_id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    registry_id TEXT NOT NULL,
    organization_id TEXT NOT NULL,
    alias_value TEXT NOT NULL CHECK (length(alias_value) BETWEEN 1 AND 512),
    person_id UUID NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (registry_id, organization_id, alias_value),
    FOREIGN KEY (person_id, registry_id, organization_id)
        REFERENCES litecr.person (person_id, registry_id, organization_id)
);

CREATE TABLE litecr.source_record (
    source_record_id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    registry_id TEXT NOT NULL,
    organization_id TEXT NOT NULL,
    source_system_id TEXT NOT NULL CHECK (length(source_system_id) BETWEEN 1 AND 256),
    source_record_key TEXT NOT NULL CHECK (length(source_record_key) BETWEEN 1 AND 512),
    person_id UUID NOT NULL,
    current_receive_version BIGINT NOT NULL DEFAULT 1 CHECK (current_receive_version > 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (registry_id, organization_id, source_system_id, source_record_key),
    UNIQUE (source_record_id, registry_id, organization_id),
    FOREIGN KEY (person_id, registry_id, organization_id)
        REFERENCES litecr.person (person_id, registry_id, organization_id)
);

CREATE INDEX source_record_person_scope_idx
    ON litecr.source_record (registry_id, organization_id, person_id);

CREATE TABLE litecr.source_record_version (
    source_record_id UUID NOT NULL REFERENCES litecr.source_record(source_record_id),
    receive_version BIGINT NOT NULL CHECK (receive_version > 0),
    source_version TEXT CHECK (source_version IS NULL OR length(source_version) BETWEEN 1 AND 512),
    payload_hash BYTEA NOT NULL CHECK (octet_length(payload_hash) = 32),
    version_state TEXT NOT NULL CHECK (version_state IN ('accepted', 'source_conflict')),
    received_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (source_record_id, receive_version)
);

CREATE TABLE litecr.source_assertion (
    source_record_id UUID NOT NULL,
    receive_version BIGINT NOT NULL,
    field_path TEXT NOT NULL CHECK (length(field_path) BETWEEN 1 AND 256),
    original_value TEXT NOT NULL CHECK (length(original_value) <= 16384),
    language_tag TEXT CHECK (language_tag IS NULL OR length(language_tag) <= 64),
    script TEXT CHECK (script IS NULL OR length(script) <= 32),
    PRIMARY KEY (source_record_id, receive_version, field_path),
    FOREIGN KEY (source_record_id, receive_version)
        REFERENCES litecr.source_record_version (source_record_id, receive_version)
);

CREATE TABLE litecr.source_identifier_assertion (
    assertion_id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    source_record_id UUID NOT NULL,
    receive_version BIGINT NOT NULL,
    registry_id TEXT NOT NULL,
    organization_id TEXT NOT NULL,
    authority TEXT NOT NULL CHECK (length(authority) BETWEEN 1 AND 2048),
    identifier_type TEXT NOT NULL CHECK (length(identifier_type) BETWEEN 1 AND 128),
    original_value TEXT NOT NULL CHECK (length(original_value) BETWEEN 1 AND 512),
    authoritative BOOLEAN NOT NULL DEFAULT FALSE,
    assertion_state TEXT NOT NULL CHECK (assertion_state IN ('accepted', 'conflict')),
    FOREIGN KEY (source_record_id, receive_version)
        REFERENCES litecr.source_record_version (source_record_id, receive_version),
    FOREIGN KEY (source_record_id, registry_id, organization_id)
        REFERENCES litecr.source_record (source_record_id, registry_id, organization_id)
);

CREATE INDEX source_assertion_scoped_path_idx
    ON litecr.source_assertion (source_record_id, receive_version, field_path);

CREATE TABLE litecr.person_identifier (
    identifier_id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    registry_id TEXT NOT NULL,
    organization_id TEXT NOT NULL,
    authority TEXT NOT NULL,
    identifier_type TEXT NOT NULL,
    identifier_value TEXT NOT NULL,
    person_id UUID NOT NULL,
    source_record_id UUID NOT NULL REFERENCES litecr.source_record(source_record_id),
    authoritative BOOLEAN NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (registry_id, authority, identifier_type, identifier_value),
    FOREIGN KEY (person_id, registry_id, organization_id)
        REFERENCES litecr.person (person_id, registry_id, organization_id)
);

CREATE TABLE litecr.registry_idempotency (
    actor_kind TEXT NOT NULL CHECK (actor_kind IN ('human', 'integration_client', 'system')),
    actor_issuer_url TEXT NOT NULL DEFAULT '',
    actor_id TEXT NOT NULL,
    operation TEXT NOT NULL,
    registry_id TEXT NOT NULL,
    idempotency_key_hash BYTEA NOT NULL CHECK (octet_length(idempotency_key_hash) = 32),
    command_fingerprint BYTEA NOT NULL CHECK (octet_length(command_fingerprint) = 32),
    person_public_id TEXT NOT NULL,
    source_record_id UUID NOT NULL REFERENCES litecr.source_record(source_record_id),
    source_receive_version BIGINT NOT NULL,
    outcome TEXT NOT NULL CHECK (outcome IN ('accepted', 'source_conflict')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (actor_kind, actor_issuer_url, actor_id, operation, registry_id, idempotency_key_hash)
);

CREATE TABLE litecr.mutation_audit (
    audit_id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    occurred_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    actor_kind TEXT NOT NULL CHECK (actor_kind IN ('human', 'integration_client', 'system')),
    actor_issuer_url TEXT NOT NULL DEFAULT '',
    actor_id TEXT NOT NULL,
    registry_id TEXT NOT NULL,
    organization_id TEXT NOT NULL,
    action TEXT NOT NULL CHECK (length(action) BETWEEN 1 AND 128),
    resource_type TEXT NOT NULL,
    resource_ref TEXT NOT NULL CHECK (length(resource_ref) <= 512),
    outcome TEXT NOT NULL CHECK (outcome IN ('accepted', 'source_conflict')),
    reason_code TEXT,
    request_id TEXT NOT NULL,
    correlation_id TEXT,
    resource_version BIGINT NOT NULL,
    policy_version TEXT NOT NULL DEFAULT 'access-v1'
);

CREATE INDEX mutation_audit_scope_time_idx
    ON litecr.mutation_audit (registry_id, organization_id, occurred_at DESC);

CREATE TABLE litecr.outbox_event (
    event_id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    event_type TEXT NOT NULL CHECK (length(event_type) BETWEEN 1 AND 128),
    schema_version INTEGER NOT NULL CHECK (schema_version > 0),
    occurred_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    registry_id TEXT NOT NULL,
    organization_id TEXT NOT NULL,
    aggregate_type TEXT NOT NULL,
    aggregate_id UUID NOT NULL,
    aggregate_version BIGINT NOT NULL CHECK (aggregate_version > 0),
    actor_kind TEXT NOT NULL CHECK (actor_kind IN ('human', 'integration_client', 'system')),
    actor_id TEXT NOT NULL,
    correlation_id TEXT,
    payload JSONB NOT NULL,
    published_at TIMESTAMPTZ,
    attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    next_attempt_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_error_code TEXT,
    CHECK (octet_length(payload::text) <= 8192)
);

CREATE INDEX outbox_pending_idx
    ON litecr.outbox_event (next_attempt_at, occurred_at)
    WHERE published_at IS NULL;

DO $$
DECLARE
    updated_rows INTEGER;
BEGIN
    UPDATE litecr.schema_meta
    SET version = 4, applied_at = now()
    WHERE singleton = TRUE AND version = 3;
    GET DIAGNOSTICS updated_rows = ROW_COUNT;
    IF updated_rows <> 1 THEN
        RAISE EXCEPTION 'liteCR schema version must be 3 before applying registry migration';
    END IF;
END $$;
