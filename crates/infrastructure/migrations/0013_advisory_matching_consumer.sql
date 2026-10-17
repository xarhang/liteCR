CREATE TABLE litecr.consumer_job (
    consumer_name TEXT NOT NULL CHECK (consumer_name = 'advisory-matcher-v1'),
    event_id UUID NOT NULL REFERENCES litecr.outbox_event(event_id),
    registry_id TEXT NOT NULL CHECK (length(registry_id) BETWEEN 1 AND 256),
    organization_id TEXT NOT NULL CHECK (length(organization_id) BETWEEN 1 AND 256),
    source_record_id UUID NOT NULL,
    expected_receive_version BIGINT NOT NULL CHECK (expected_receive_version > 0),
    state TEXT NOT NULL DEFAULT 'pending' CHECK (state IN ('pending','leased','completed','obsolete','failed')),
    attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts BETWEEN 0 AND 12),
    available_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    lease_owner TEXT CHECK (lease_owner IS NULL OR length(lease_owner) BETWEEN 1 AND 128),
    lease_token UUID,
    lease_expires_at TIMESTAMPTZ,
    last_error_code TEXT CHECK (last_error_code IS NULL OR last_error_code IN ('TRANSIENT_DATABASE','EVIDENCE_UNAVAILABLE','RETRY_EXHAUSTED')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (consumer_name, event_id),
    FOREIGN KEY (source_record_id, registry_id, organization_id)
        REFERENCES litecr.source_record (source_record_id, registry_id, organization_id),
    CHECK ((state = 'leased' AND lease_owner IS NOT NULL AND lease_token IS NOT NULL AND lease_expires_at IS NOT NULL)
        OR (state <> 'leased' AND lease_owner IS NULL AND lease_token IS NULL AND lease_expires_at IS NULL)),
    CHECK (state <> 'completed' OR attempts > 0)
);

CREATE INDEX consumer_job_claim_idx
    ON litecr.consumer_job (available_at, created_at)
    WHERE state IN ('pending','leased');

CREATE TABLE litecr.consumer_receipt (
    consumer_name TEXT NOT NULL CHECK (consumer_name = 'advisory-matcher-v1'),
    event_id UUID NOT NULL,
    outcome TEXT NOT NULL CHECK (outcome IN ('completed','obsolete','failed')),
    evidence_run_id UUID UNIQUE REFERENCES matching.evidence_run(run_id),
    receipt_code TEXT CHECK (receipt_code IS NULL OR receipt_code IN ('SOURCE_SUPERSEDED','SOURCE_WITHDRAWN','SOURCE_SCOPE_MISMATCH','SOURCE_MALFORMED','RETRY_EXHAUSTED')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (consumer_name, event_id),
    FOREIGN KEY (consumer_name, event_id)
        REFERENCES litecr.consumer_job (consumer_name, event_id),
    CHECK ((outcome = 'completed') = (evidence_run_id IS NOT NULL)),
    CHECK ((outcome = 'completed') = (receipt_code IS NULL))
);

CREATE FUNCTION litecr.enqueue_advisory_matching_event() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.event_type = 'registry.source_record.accepted'
       AND NEW.schema_version = 1
       AND NEW.aggregate_type = 'source_record'
       AND NEW.aggregate_version > 0 THEN
        INSERT INTO litecr.consumer_job (
            consumer_name, event_id, registry_id, organization_id,
            source_record_id, expected_receive_version
        ) VALUES (
            'advisory-matcher-v1', NEW.event_id, NEW.registry_id,
            NEW.organization_id, NEW.aggregate_id, NEW.aggregate_version
        ) ON CONFLICT (consumer_name, event_id) DO NOTHING;
    END IF;
    RETURN NEW;
END $$;

CREATE TRIGGER enqueue_advisory_matching_event_after_outbox_insert
AFTER INSERT ON litecr.outbox_event
FOR EACH ROW EXECUTE FUNCTION litecr.enqueue_advisory_matching_event();

INSERT INTO litecr.consumer_job (
    consumer_name, event_id, registry_id, organization_id,
    source_record_id, expected_receive_version
)
SELECT 'advisory-matcher-v1', event_id, registry_id, organization_id,
       aggregate_id, aggregate_version
FROM litecr.outbox_event
WHERE event_type = 'registry.source_record.accepted'
  AND schema_version = 1
  AND aggregate_type = 'source_record'
ON CONFLICT (consumer_name, event_id) DO NOTHING;

DO $$
DECLARE updated_rows INTEGER;
BEGIN
    UPDATE litecr.schema_meta SET version = 13, applied_at = now()
    WHERE singleton = TRUE AND version = 12;
    GET DIAGNOSTICS updated_rows = ROW_COUNT;
    IF updated_rows <> 1 THEN
        RAISE EXCEPTION 'liteCR schema version must be 12 before applying advisory matching consumer migration';
    END IF;
END $$;
