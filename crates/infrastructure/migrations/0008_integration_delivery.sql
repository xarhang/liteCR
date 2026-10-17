CREATE TABLE litecr.integration_delivery (
    delivery_id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    event_id UUID NOT NULL REFERENCES litecr.outbox_event(event_id),
    recipient_key TEXT NOT NULL CHECK (length(recipient_key) BETWEEN 1 AND 256),
    state TEXT NOT NULL DEFAULT 'pending'
        CHECK (state IN ('pending', 'leased', 'delivered', 'retryable', 'blocked')),
    attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    next_attempt_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    lease_until TIMESTAMPTZ,
    delivered_at TIMESTAMPTZ,
    last_error_code TEXT CHECK (last_error_code IS NULL OR length(last_error_code) <= 128),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (event_id, recipient_key),
    CHECK ((state = 'leased') = (lease_until IS NOT NULL)),
    CHECK ((state = 'delivered') = (delivered_at IS NOT NULL))
);

CREATE INDEX integration_delivery_claim_idx
    ON litecr.integration_delivery (next_attempt_at, created_at, delivery_id)
    WHERE state IN ('pending', 'retryable');

DO $$
DECLARE updated_rows INTEGER;
BEGIN
    UPDATE litecr.schema_meta
    SET version = 8, applied_at = now()
    WHERE singleton = TRUE AND version = 7;
    GET DIAGNOSTICS updated_rows = ROW_COUNT;
    IF updated_rows <> 1 THEN
        RAISE EXCEPTION 'liteCR schema version must be 7 before applying integration delivery migration';
    END IF;
END $$;
