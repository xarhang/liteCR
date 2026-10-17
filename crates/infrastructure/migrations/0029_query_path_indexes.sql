-- Query-path indexes, not new identity or uniqueness constraints.
-- UUID-leading dependency indexes include historical/inactive relationships:
-- reversal safety must not lose a dependency merely because it is inactive.
-- Fixed-size value digest bounds the index tuple; queries retain full equality.
CREATE INDEX person_alias_person_scope_idx
    ON litecr.person_alias (person_id,registry_id,organization_id);

CREATE INDEX person_redirect_target_lookup_idx
    ON litecr.person_redirect (target_person_id,registry_id,organization_id,source_person_id);

CREATE INDEX person_identifier_person_scope_idx
    ON litecr.person_identifier (registry_id,organization_id,person_id);

CREATE INDEX source_identifier_accepted_value_idx
    ON litecr.source_identifier_assertion (registry_id,organization_id,md5(original_value)) WHERE assertion_state='accepted' AND withdrawn_at IS NULL;

CREATE INDEX source_record_person_lookup_idx
    ON litecr.source_record (person_id);

CREATE INDEX review_case_scope_cursor_idx
    ON litecr.review_case (registry_id,organization_id,created_at DESC,case_id DESC);

CREATE INDEX migration_journal_batch_lookup_idx
    ON litecr.migration_journal (manifest_sha256,batch_sequence);

CREATE INDEX migration_journal_imported_source_idx
    ON litecr.migration_journal (manifest_sha256,imported_source_record_id,imported_receive_version);

CREATE INDEX review_case_query_source_lookup_idx
    ON litecr.review_case (query_source_record_id);

CREATE INDEX review_case_candidate_source_lookup_idx
    ON litecr.review_case (candidate_source_record_id);

CREATE INDEX identity_merge_member_source_lookup_idx
    ON litecr.identity_merge_member (source_record_id);

CREATE INDEX confirmed_non_match_left_lookup_idx
    ON litecr.confirmed_non_match (left_source_record_id);

CREATE INDEX confirmed_non_match_right_lookup_idx
    ON litecr.confirmed_non_match (right_source_record_id);

CREATE INDEX identity_link_source_lookup_idx
    ON litecr.identity_link (source_person_id);

CREATE INDEX identity_link_target_lookup_idx
    ON litecr.identity_link (target_person_id);

CREATE INDEX identity_merge_source_lookup_idx
    ON litecr.identity_merge (source_person_id);

CREATE INDEX identity_merge_target_lookup_idx
    ON litecr.identity_merge (target_person_id);

CREATE INDEX person_redirect_source_lookup_idx
    ON litecr.person_redirect (source_person_id);

-- Same columns/order/predicate as source_assertion's primary-key index.
-- Keep the constraint index; remove only the redundant non-unique copy.
DROP INDEX litecr.source_assertion_scoped_path_idx;

DO $$
DECLARE updated_rows INTEGER;
BEGIN
    UPDATE litecr.schema_meta SET version=29, applied_at=now()
    WHERE singleton=TRUE AND version=28;
    GET DIAGNOSTICS updated_rows=ROW_COUNT;
    IF updated_rows<>1 THEN
        RAISE EXCEPTION 'liteCR schema version must be 28 before applying query-path indexes';
    END IF;
END $$;
