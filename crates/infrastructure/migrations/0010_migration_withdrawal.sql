ALTER TABLE litecr.source_record
    ADD COLUMN withdrawn_at TIMESTAMPTZ,
    ADD COLUMN withdrawn_by_manifest BYTEA
        CHECK (withdrawn_by_manifest IS NULL OR octet_length(withdrawn_by_manifest) = 32),
    ADD CONSTRAINT source_record_withdrawal_pair_check CHECK
        ((withdrawn_at IS NULL) = (withdrawn_by_manifest IS NULL));

ALTER TABLE litecr.source_record_version
    ADD COLUMN withdrawn_at TIMESTAMPTZ,
    ADD COLUMN withdrawn_by_manifest BYTEA
        CHECK (withdrawn_by_manifest IS NULL OR octet_length(withdrawn_by_manifest) = 32),
    ADD CONSTRAINT source_record_version_withdrawal_pair_check CHECK
        ((withdrawn_at IS NULL) = (withdrawn_by_manifest IS NULL));

ALTER TABLE litecr.source_assertion
    ADD COLUMN withdrawn_at TIMESTAMPTZ,
    ADD COLUMN withdrawn_by_manifest BYTEA
        CHECK (withdrawn_by_manifest IS NULL OR octet_length(withdrawn_by_manifest) = 32),
    ADD CONSTRAINT source_assertion_withdrawal_pair_check CHECK
        ((withdrawn_at IS NULL) = (withdrawn_by_manifest IS NULL));

ALTER TABLE litecr.source_identifier_assertion
    ADD COLUMN withdrawn_at TIMESTAMPTZ,
    ADD COLUMN withdrawn_by_manifest BYTEA
        CHECK (withdrawn_by_manifest IS NULL OR octet_length(withdrawn_by_manifest) = 32),
    ADD CONSTRAINT source_identifier_assertion_withdrawal_pair_check CHECK
        ((withdrawn_at IS NULL) = (withdrawn_by_manifest IS NULL));

ALTER TABLE litecr.migration_journal
    ADD COLUMN source_record_created BOOLEAN NOT NULL DEFAULT FALSE,
    ADD COLUMN person_created BOOLEAN NOT NULL DEFAULT FALSE,
    ADD COLUMN previous_receive_version BIGINT CHECK
        (previous_receive_version IS NULL OR previous_receive_version > 0);

CREATE TABLE litecr.migration_effect (
    effect_id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    manifest_sha256 BYTEA NOT NULL REFERENCES litecr.migration_checkpoint(manifest_sha256),
    source_record_id UUID NOT NULL REFERENCES litecr.source_record(source_record_id),
    effect_kind TEXT NOT NULL CHECK (effect_kind IN ('person_alias','person_identifier')),
    effect_ref UUID NOT NULL,
    disposition TEXT NOT NULL DEFAULT 'owned' CHECK (disposition IN ('owned','detached','retained')),
    resolution_code TEXT CHECK (resolution_code IS NULL OR length(resolution_code) <= 128),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (manifest_sha256, source_record_id, effect_kind, effect_ref),
    CHECK (octet_length(manifest_sha256) = 32)
);

CREATE INDEX migration_effect_source_idx
    ON litecr.migration_effect (manifest_sha256, source_record_id, effect_kind);

CREATE TABLE litecr.migration_reverse_receipt (
    manifest_sha256 BYTEA PRIMARY KEY REFERENCES litecr.migration_checkpoint(manifest_sha256),
    registry_id TEXT NOT NULL CHECK (length(registry_id) BETWEEN 1 AND 256),
    organization_id TEXT NOT NULL CHECK (length(organization_id) BETWEEN 1 AND 256),
    reversed_by TEXT NOT NULL CHECK (length(reversed_by) BETWEEN 1 AND 512),
    reason TEXT NOT NULL CHECK (length(reason) BETWEEN 1 AND 2000),
    withdrawn_record_count BIGINT NOT NULL CHECK (withdrawn_record_count >= 0),
    detached_identifier_count BIGINT NOT NULL CHECK (detached_identifier_count >= 0),
    retained_alias_count BIGINT NOT NULL CHECK (retained_alias_count >= 0),
    reversed_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (octet_length(manifest_sha256) = 32)
);

DO $$
DECLARE updated_rows INTEGER;
BEGIN
    UPDATE litecr.schema_meta SET version = 10, applied_at = now()
    WHERE singleton = TRUE AND version = 9;
    GET DIAGNOSTICS updated_rows = ROW_COUNT;
    IF updated_rows <> 1 THEN
        RAISE EXCEPTION 'liteCR schema version must be 9 before applying migration withdrawal migration';
    END IF;
END $$;
