CREATE TABLE litecr.review_case (
    case_id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    registry_id TEXT NOT NULL,
    organization_id TEXT NOT NULL,
    query_source_record_id UUID NOT NULL,
    candidate_source_record_id UUID NOT NULL,
    query_receive_version BIGINT NOT NULL CHECK (query_receive_version > 0),
    candidate_receive_version BIGINT NOT NULL CHECK (candidate_receive_version > 0),
    case_version BIGINT NOT NULL DEFAULT 1 CHECK (case_version > 0),
    state TEXT NOT NULL DEFAULT 'open' CHECK (state IN ('open','assigned','decided','closed')),
    assigned_actor_id TEXT,
    created_by TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (query_source_record_id <> candidate_source_record_id),
    UNIQUE (registry_id, organization_id, query_source_record_id, candidate_source_record_id, query_receive_version, candidate_receive_version),
    FOREIGN KEY (query_source_record_id, registry_id, organization_id)
        REFERENCES litecr.source_record (source_record_id, registry_id, organization_id),
    FOREIGN KEY (candidate_source_record_id, registry_id, organization_id)
        REFERENCES litecr.source_record (source_record_id, registry_id, organization_id)
);

CREATE INDEX review_case_queue_idx ON litecr.review_case
    (registry_id, organization_id, state, created_at, case_id);

CREATE TABLE litecr.review_decision (
    decision_id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    case_id UUID NOT NULL REFERENCES litecr.review_case(case_id),
    case_version BIGINT NOT NULL,
    decision TEXT NOT NULL CHECK (decision IN ('confirmed_non_match','link','merge','reverse_merge')),
    actor_id TEXT NOT NULL,
    actor_issuer_url TEXT NOT NULL DEFAULT '',
    reason_code TEXT NOT NULL CHECK (length(reason_code) BETWEEN 1 AND 128),
    reason TEXT NOT NULL CHECK (length(reason) BETWEEN 1 AND 2000),
    idempotency_key_hash BYTEA NOT NULL CHECK (octet_length(idempotency_key_hash) = 32),
    command_fingerprint BYTEA NOT NULL CHECK (octet_length(command_fingerprint) = 32),
    merge_id UUID,
    outcome TEXT NOT NULL CHECK (outcome IN ('accepted','stale','conflict','requires_review')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (case_id, actor_issuer_url, actor_id, idempotency_key_hash)
);

CREATE TABLE litecr.confirmed_non_match (
    registry_id TEXT NOT NULL,
    organization_id TEXT NOT NULL,
    left_source_record_id UUID NOT NULL,
    right_source_record_id UUID NOT NULL,
    decision_id UUID NOT NULL UNIQUE REFERENCES litecr.review_decision(decision_id),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (left_source_record_id < right_source_record_id),
    PRIMARY KEY (registry_id, organization_id, left_source_record_id, right_source_record_id),
    FOREIGN KEY (left_source_record_id, registry_id, organization_id)
        REFERENCES litecr.source_record (source_record_id, registry_id, organization_id),
    FOREIGN KEY (right_source_record_id, registry_id, organization_id)
        REFERENCES litecr.source_record (source_record_id, registry_id, organization_id)
);

CREATE TABLE litecr.identity_merge (
    merge_id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    registry_id TEXT NOT NULL,
    organization_id TEXT NOT NULL,
    source_person_id UUID NOT NULL,
    target_person_id UUID NOT NULL,
    decision_id UUID NOT NULL UNIQUE REFERENCES litecr.review_decision(decision_id),
    source_version_before BIGINT NOT NULL,
    target_version_before BIGINT NOT NULL,
    source_version_after BIGINT NOT NULL,
    target_version_after BIGINT NOT NULL,
    reversed_by UUID REFERENCES litecr.review_decision(decision_id),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (source_person_id <> target_person_id),
    UNIQUE (merge_id, registry_id, organization_id),
    FOREIGN KEY (source_person_id, registry_id, organization_id) REFERENCES litecr.person(person_id, registry_id, organization_id),
    FOREIGN KEY (target_person_id, registry_id, organization_id) REFERENCES litecr.person(person_id, registry_id, organization_id)
);

CREATE INDEX identity_merge_target_idx ON litecr.identity_merge
    (registry_id, organization_id, target_person_id, created_at DESC);

CREATE TABLE litecr.person_redirect (
    registry_id TEXT NOT NULL,
    organization_id TEXT NOT NULL,
    redirect_id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    source_person_id UUID NOT NULL,
    target_person_id UUID NOT NULL,
    merge_id UUID NOT NULL UNIQUE REFERENCES litecr.identity_merge(merge_id),
    active BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (source_person_id <> target_person_id),
    FOREIGN KEY (source_person_id, registry_id, organization_id) REFERENCES litecr.person(person_id, registry_id, organization_id),
    FOREIGN KEY (target_person_id, registry_id, organization_id) REFERENCES litecr.person(person_id, registry_id, organization_id)
);
CREATE UNIQUE INDEX person_redirect_active_source_idx ON litecr.person_redirect (registry_id,organization_id,source_person_id) WHERE active=TRUE;

CREATE TABLE litecr.identity_link (
    link_id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    registry_id TEXT NOT NULL,
    organization_id TEXT NOT NULL,
    source_person_id UUID NOT NULL,
    target_person_id UUID NOT NULL,
    decision_id UUID NOT NULL UNIQUE REFERENCES litecr.review_decision(decision_id),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (source_person_id <> target_person_id),
    FOREIGN KEY (source_person_id, registry_id, organization_id) REFERENCES litecr.person(person_id, registry_id, organization_id),
    FOREIGN KEY (target_person_id, registry_id, organization_id) REFERENCES litecr.person(person_id, registry_id, organization_id)
);

CREATE TABLE litecr.identity_merge_member (
    merge_id UUID NOT NULL REFERENCES litecr.identity_merge(merge_id),
    source_record_id UUID NOT NULL REFERENCES litecr.source_record(source_record_id),
    receive_version BIGINT NOT NULL,
    PRIMARY KEY (merge_id, source_record_id)
);

DO $$
DECLARE updated_rows INTEGER;
BEGIN
    UPDATE litecr.schema_meta SET version = 7, applied_at = now()
    WHERE singleton = TRUE AND version = 6;
    GET DIAGNOSTICS updated_rows = ROW_COUNT;
    IF updated_rows <> 1 THEN
        RAISE EXCEPTION 'liteCR schema version must be 6 before applying review migration';
    END IF;
END $$;
