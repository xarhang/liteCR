-- Quiesced, transactional maintenance conversion; not an online migration.
-- Keep authoritative facts/evidence unchanged. Hash by the existing PK's UUID
-- so marker probes can prune without adding a partition key to any contract.
LOCK TABLE matching.indexed_source_version IN ACCESS EXCLUSIVE MODE;

DO $$
DECLARE
    original_oid oid := 'matching.indexed_source_version'::regclass;
    owner_oid oid := (SELECT oid FROM pg_roles WHERE rolname=current_user);
    original_acl aclitem[];
    original_pk name;
    original_fk name;
    entry record;
    part integer;
    updated_rows integer;
BEGIN
    IF NOT EXISTS (SELECT 1 FROM litecr.schema_meta WHERE singleton AND version=29) THEN
        RAISE EXCEPTION 'PARTITION_MARKER_SCHEMA_29_REQUIRED';
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_class WHERE oid=original_oid
        AND relkind='r' AND relowner=owner_oid AND NOT relrowsecurity AND NOT relforcerowsecurity
        AND reloptions IS NULL AND reltablespace=0 AND relpersistence='p' AND relreplident='d') THEN
        RAISE EXCEPTION 'PARTITION_MARKER_OWNER_OR_TABLE_POLICY';
    END IF;
    IF EXISTS (SELECT 1 FROM pg_attribute WHERE attrelid=original_oid AND attacl IS NOT NULL)
       OR EXISTS (SELECT 1 FROM pg_trigger WHERE tgrelid=original_oid AND NOT tgisinternal)
       OR EXISTS (SELECT 1 FROM pg_constraint WHERE confrelid=original_oid)
       OR EXISTS (SELECT 1 FROM pg_policy WHERE polrelid=original_oid)
       OR EXISTS (SELECT 1 FROM pg_rewrite WHERE ev_class=original_oid)
       OR EXISTS (SELECT 1 FROM pg_inherits WHERE inhparent=original_oid OR inhrelid=original_oid)
       OR EXISTS (SELECT 1 FROM pg_seclabel WHERE classoid='pg_class'::regclass AND objoid=original_oid)
       OR EXISTS (SELECT 1 FROM pg_publication_rel WHERE prrelid=original_oid)
       OR EXISTS (SELECT 1 FROM pg_publication WHERE puballtables)
       OR EXISTS (SELECT 1 FROM pg_publication_namespace
           WHERE pnnspid=(SELECT relnamespace FROM pg_class WHERE oid=original_oid))
       OR EXISTS (SELECT 1 FROM pg_subscription_rel WHERE srrelid=original_oid)
       OR (SELECT count(*) FROM pg_constraint WHERE conrelid=original_oid AND contype<>'n')<>2 THEN
        RAISE EXCEPTION 'PARTITION_MARKER_UNEXPECTED_DEPENDENCY_OR_POLICY';
    END IF;
    IF (SELECT count(*) FROM pg_attribute WHERE attrelid=original_oid AND attnum>0 AND NOT attisdropped)<>4
       OR NOT EXISTS (SELECT 1 FROM pg_attribute WHERE attrelid=original_oid AND attname='source_record_id'
           AND atttypid='uuid'::regtype AND attnotnull AND attidentity='' AND attgenerated='')
       OR NOT EXISTS (SELECT 1 FROM pg_attribute WHERE attrelid=original_oid AND attname='receive_version'
           AND atttypid='bigint'::regtype AND attnotnull AND attidentity='' AND attgenerated='')
       OR NOT EXISTS (SELECT 1 FROM pg_attribute WHERE attrelid=original_oid AND attname='normalizer_version'
           AND atttypid='text'::regtype AND attnotnull AND attidentity='' AND attgenerated='')
       OR NOT EXISTS (SELECT 1 FROM pg_attribute WHERE attrelid=original_oid AND attname='indexed_at'
           AND atttypid='timestamptz'::regtype AND attnotnull AND attidentity='' AND attgenerated='') THEN
        RAISE EXCEPTION 'PARTITION_MARKER_UNEXPECTED_COLUMNS';
    END IF;
    SELECT conname INTO STRICT original_pk FROM pg_constraint
        WHERE conrelid=original_oid AND contype='p'
          AND NOT condeferrable AND NOT condeferred AND convalidated
          AND ARRAY(SELECT a.attname::text FROM unnest(conkey) WITH ORDINALITY k(attnum,pos)
              JOIN pg_attribute a ON a.attrelid=original_oid AND a.attnum=k.attnum ORDER BY k.pos)
              =ARRAY['source_record_id','receive_version','normalizer_version'];
    SELECT conname INTO STRICT original_fk FROM pg_constraint
        WHERE conrelid=original_oid AND contype='f'
          AND confrelid='litecr.source_record_version'::regclass
          AND NOT condeferrable AND NOT condeferred AND convalidated
          AND confupdtype='a' AND confdeltype='a' AND confmatchtype='s'
          AND ARRAY(SELECT a.attname::text FROM unnest(conkey) WITH ORDINALITY k(attnum,pos)
              JOIN pg_attribute a ON a.attrelid=original_oid AND a.attnum=k.attnum ORDER BY k.pos)
              =ARRAY['source_record_id','receive_version']
          AND ARRAY(SELECT a.attname::text FROM unnest(confkey) WITH ORDINALITY k(attnum,pos)
              JOIN pg_attribute a ON a.attrelid=confrelid AND a.attnum=k.attnum ORDER BY k.pos)
              =ARRAY['source_record_id','receive_version'];
    IF (SELECT count(*) FROM pg_index WHERE indrelid=original_oid)<>2
       OR NOT EXISTS (SELECT 1 FROM pg_index i JOIN pg_class c ON c.oid=i.indexrelid
           JOIN pg_am am ON am.oid=c.relam
           WHERE i.indrelid=original_oid AND c.relname='indexed_source_version_version_idx'
             AND c.relowner=owner_oid AND c.reloptions IS NULL AND c.reltablespace=0
             AND am.amname='btree' AND NOT i.indisunique AND i.indisvalid AND i.indisready
             AND i.indnkeyatts=3 AND i.indnatts=3 AND i.indpred IS NULL AND i.indexprs IS NULL
             AND pg_get_indexdef(i.indexrelid,1,TRUE)='normalizer_version'
             AND pg_get_indexdef(i.indexrelid,2,TRUE)='source_record_id'
             AND pg_get_indexdef(i.indexrelid,3,TRUE)='receive_version')
       OR EXISTS (SELECT 1 FROM pg_index i JOIN pg_class c ON c.oid=i.indexrelid
           WHERE i.indrelid=original_oid AND (c.relowner<>owner_oid OR c.reloptions IS NOT NULL
             OR c.reltablespace<>0 OR NOT i.indisvalid OR NOT i.indisready))
       OR NOT EXISTS (SELECT 1 FROM pg_index i JOIN pg_class c ON c.oid=i.indexrelid
           JOIN pg_am am ON am.oid=c.relam WHERE i.indrelid=original_oid AND i.indisprimary
             AND am.amname='btree' AND i.indnkeyatts=3 AND i.indnatts=3
             AND i.indpred IS NULL AND i.indexprs IS NULL
             AND pg_get_indexdef(i.indexrelid,1,TRUE)='source_record_id'
             AND pg_get_indexdef(i.indexrelid,2,TRUE)='receive_version'
             AND pg_get_indexdef(i.indexrelid,3,TRUE)='normalizer_version') THEN
        RAISE EXCEPTION 'PARTITION_MARKER_UNEXPECTED_INDEX';
    END IF;
    SELECT COALESCE(relacl,acldefault('r',relowner)) INTO original_acl
        FROM pg_class WHERE oid=original_oid;
    IF EXISTS (SELECT 1 FROM aclexplode(original_acl) WHERE grantor<>owner_oid) THEN
        RAISE EXCEPTION 'PARTITION_MARKER_EXTERNAL_ACL_GRANTOR';
    END IF;

    CREATE TABLE matching.indexed_source_version_partitioned_new (
        LIKE matching.indexed_source_version INCLUDING DEFAULTS INCLUDING CONSTRAINTS,
        CONSTRAINT indexed_source_version_v30_pkey
            PRIMARY KEY(source_record_id,receive_version,normalizer_version),
        CONSTRAINT indexed_source_version_v30_source_version_fkey
            FOREIGN KEY(source_record_id,receive_version)
            REFERENCES litecr.source_record_version(source_record_id,receive_version)
    ) PARTITION BY HASH(source_record_id);
    FOR part IN 0..7 LOOP
        EXECUTE format(
            'CREATE TABLE matching.indexed_source_version_p%s PARTITION OF matching.indexed_source_version_partitioned_new FOR VALUES WITH (MODULUS 8,REMAINDER %s)',
            part,part);
    END LOOP;
    CREATE INDEX indexed_source_version_v30_version_idx
        ON matching.indexed_source_version_partitioned_new(normalizer_version,source_record_id,receive_version);
    INSERT INTO matching.indexed_source_version_partitioned_new
        (source_record_id,receive_version,normalizer_version,indexed_at)
        SELECT source_record_id,receive_version,normalizer_version,indexed_at
        FROM matching.indexed_source_version;
    IF EXISTS (SELECT * FROM matching.indexed_source_version
               EXCEPT ALL SELECT * FROM matching.indexed_source_version_partitioned_new)
       OR EXISTS (SELECT * FROM matching.indexed_source_version_partitioned_new
                  EXCEPT ALL SELECT * FROM matching.indexed_source_version) THEN
        RAISE EXCEPTION 'PARTITION_MARKER_COPY_MISMATCH';
    END IF;

    -- New objects may inherit operator/default grants. Remove every non-owner
    -- direct grant on this exact new tree; parent grants are restored below.
    FOR entry IN
        SELECT DISTINCT c.oid::regclass AS relation,a.grantee
        FROM pg_class c
        CROSS JOIN LATERAL aclexplode(COALESCE(c.relacl,acldefault('r',c.relowner))) a
        WHERE (c.oid='matching.indexed_source_version_partitioned_new'::regclass
          OR c.oid IN (SELECT inhrelid FROM pg_inherits
                      WHERE inhparent='matching.indexed_source_version_partitioned_new'::regclass))
          AND a.grantee<>owner_oid
    LOOP
        EXECUTE format('REVOKE ALL PRIVILEGES ON TABLE %s FROM %s',entry.relation,
            CASE WHEN entry.grantee=0 THEN 'PUBLIC'
                 ELSE quote_ident(pg_get_userbyid(entry.grantee)) END);
    END LOOP;
    -- RESTRICT fails closed on an unexpected view/dependency; SQLx rolls back
    -- all copy/DDL/ACL changes together with schema_meta and its migration ledger.
    DROP TABLE matching.indexed_source_version RESTRICT;
    ALTER TABLE matching.indexed_source_version_partitioned_new RENAME TO indexed_source_version;
    EXECUTE format('ALTER TABLE matching.indexed_source_version RENAME CONSTRAINT indexed_source_version_v30_pkey TO %I',original_pk);
    EXECUTE format('ALTER TABLE matching.indexed_source_version RENAME CONSTRAINT indexed_source_version_v30_source_version_fkey TO %I',original_fk);
    ALTER INDEX matching.indexed_source_version_v30_version_idx RENAME TO indexed_source_version_version_idx;

    -- Recreate the old owner-issued direct parent ACL, including grant options.
    -- Unexpected external grant chains were rejected before copying any rows.
    REVOKE ALL PRIVILEGES ON TABLE matching.indexed_source_version FROM CURRENT_USER;
    FOR entry IN SELECT * FROM aclexplode(original_acl) LOOP
        EXECUTE format('GRANT %s ON TABLE matching.indexed_source_version TO %s%s',
            entry.privilege_type,
            CASE WHEN entry.grantee=0 THEN 'PUBLIC'
                 ELSE quote_ident(pg_get_userbyid(entry.grantee)) END,
            CASE WHEN entry.is_grantable THEN ' WITH GRANT OPTION' ELSE '' END);
    END LOOP;
    UPDATE litecr.schema_meta SET version=30,applied_at=now() WHERE singleton AND version=29;
    GET DIAGNOSTICS updated_rows=ROW_COUNT;
    IF updated_rows<>1 THEN RAISE EXCEPTION 'PARTITION_MARKER_SCHEMA_UPDATE_FAILED'; END IF;
END $$;
