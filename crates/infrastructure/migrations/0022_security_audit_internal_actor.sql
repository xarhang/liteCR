ALTER TABLE litecr.access_identity
    ADD COLUMN identity_key BIGINT GENERATED ALWAYS AS IDENTITY;

ALTER TABLE litecr.access_identity
    ADD CONSTRAINT access_identity_identity_key_unique UNIQUE (identity_key),
    ADD CONSTRAINT access_identity_identity_key_kind_unique UNIQUE (identity_key, actor_kind);

ALTER TABLE litecr.access_security_audit
    ADD COLUMN actor_identity_key BIGINT;

-- Existing 0021 audit rows may contain external subjects. Do not attempt to
-- guess their issuer namespace: discard those actor strings during this upgrade.
DROP TRIGGER access_security_audit_immutable ON litecr.access_security_audit;

DO $$
DECLARE constraint_row RECORD;
BEGIN
    FOR constraint_row IN
        SELECT conname
        FROM pg_constraint
        WHERE conrelid = 'litecr.access_security_audit'::regclass
          AND contype = 'c'
          AND pg_get_constraintdef(oid) LIKE '%actor_id%'
    LOOP
        EXECUTE format(
            'ALTER TABLE litecr.access_security_audit DROP CONSTRAINT %I',
            constraint_row.conname
        );
    END LOOP;
END $$;

UPDATE litecr.access_security_audit SET actor_kind = NULL;

ALTER TABLE litecr.access_security_audit
    DROP COLUMN actor_id,
    ADD CONSTRAINT access_security_audit_actor_pair_check
        CHECK ((actor_kind IS NULL) = (actor_identity_key IS NULL)),
    ADD CONSTRAINT access_security_audit_actor_identity_fk
        FOREIGN KEY (actor_identity_key, actor_kind)
        REFERENCES litecr.access_identity (identity_key, actor_kind);

CREATE TRIGGER access_security_audit_immutable
    BEFORE UPDATE OR DELETE ON litecr.access_security_audit
    FOR EACH ROW EXECUTE FUNCTION litecr.prevent_access_security_audit_mutation();

DO $$
DECLARE updated_rows INTEGER;
BEGIN
    UPDATE litecr.schema_meta SET version = 22, applied_at = now()
    WHERE singleton = TRUE AND version = 21;
    GET DIAGNOSTICS updated_rows = ROW_COUNT;
    IF updated_rows <> 1 THEN
        RAISE EXCEPTION 'liteCR schema version must be 21 before internal audit actor migration';
    END IF;
END $$;
