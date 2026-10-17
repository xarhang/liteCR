-- Run after `litecr migrate`, connected as the dedicated litecr_migrator role.
-- The DBA provisions litecr_worker separately. This script never creates a role,
-- changes a password, or changes database/object ownership.
\set ON_ERROR_STOP on

DO $$
DECLARE
    migration_oid oid;
    worker_oid oid;
BEGIN
    IF current_user <> 'litecr_migrator' THEN
        RAISE EXCEPTION 'WORKER_ROLE_PREFLIGHT_MIGRATION_LOGIN';
    END IF;
    IF (SELECT pg_get_userbyid(datdba) FROM pg_database WHERE datname=current_database()) <> current_user THEN
        RAISE EXCEPTION 'WORKER_ROLE_PREFLIGHT_DATABASE_OWNER';
    END IF;
    SELECT oid INTO migration_oid FROM pg_roles WHERE rolname='litecr_migrator'
      AND rolcanlogin AND NOT rolsuper AND NOT rolcreatedb AND NOT rolcreaterole
      AND NOT rolreplication AND NOT rolbypassrls;
    SELECT oid INTO worker_oid FROM pg_roles WHERE rolname='litecr_worker'
      AND rolcanlogin AND NOT rolsuper AND NOT rolcreatedb AND NOT rolcreaterole
      AND NOT rolreplication AND NOT rolbypassrls;
    IF migration_oid IS NULL OR worker_oid IS NULL THEN
        RAISE EXCEPTION 'WORKER_ROLE_PREFLIGHT_ROLE_ATTRIBUTES';
    END IF;
    IF EXISTS (SELECT 1 FROM pg_auth_members WHERE member=worker_oid) THEN
        RAISE EXCEPTION 'WORKER_ROLE_PREFLIGHT_MEMBERSHIP';
    END IF;
    IF EXISTS (SELECT 1 FROM pg_auth_members WHERE roleid=worker_oid) THEN
        RAISE EXCEPTION 'WORKER_ROLE_PREFLIGHT_HAS_MEMBERS';
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_namespace WHERE nspname='litecr' AND nspowner=migration_oid)
       OR NOT EXISTS (SELECT 1 FROM pg_namespace WHERE nspname='matching' AND nspowner=migration_oid) THEN
        RAISE EXCEPTION 'WORKER_ROLE_PREFLIGHT_SCHEMA_OWNER';
    END IF;
    IF NOT EXISTS (SELECT 1 FROM litecr.schema_meta WHERE singleton AND version IN (22,23,24,27,28,29,30,31)) THEN
        RAISE EXCEPTION 'WORKER_ROLE_PREFLIGHT_SCHEMA_VERSION';
    END IF;
    IF to_regclass('public._sqlx_migrations') IS NULL
       OR (SELECT relowner FROM pg_class WHERE oid='public._sqlx_migrations'::regclass) <> migration_oid THEN
        RAISE EXCEPTION 'WORKER_ROLE_PREFLIGHT_SQLX_LEDGER';
    END IF;
END $$;

DO $$ BEGIN
  EXECUTE format('GRANT CONNECT ON DATABASE %I TO litecr_worker', current_database());
END $$;

-- Remove any direct object grants for this role in the application schemas,
-- then grant only the worker's queue and evidence contract. This does not revoke
-- grants to other principals or alter unrelated schemas/databases.
DO $$ BEGIN
  EXECUTE format('REVOKE CREATE ON DATABASE %I FROM litecr_worker', current_database());
END $$;
REVOKE CREATE ON SCHEMA public, litecr, matching FROM litecr_worker;
REVOKE ALL PRIVILEGES ON ALL TABLES IN SCHEMA litecr, matching FROM litecr_worker;
REVOKE ALL PRIVILEGES ON ALL SEQUENCES IN SCHEMA litecr, matching FROM litecr_worker;
ALTER DEFAULT PRIVILEGES FOR ROLE litecr_migrator IN SCHEMA litecr REVOKE ALL ON TABLES FROM litecr_worker;
ALTER DEFAULT PRIVILEGES FOR ROLE litecr_migrator IN SCHEMA matching REVOKE ALL ON TABLES FROM litecr_worker;
ALTER DEFAULT PRIVILEGES FOR ROLE litecr_migrator IN SCHEMA litecr REVOKE ALL ON SEQUENCES FROM litecr_worker;
ALTER DEFAULT PRIVILEGES FOR ROLE litecr_migrator IN SCHEMA matching REVOKE ALL ON SEQUENCES FROM litecr_worker;

GRANT USAGE ON SCHEMA litecr, matching TO litecr_worker;
GRANT SELECT ON TABLE
    litecr.consumer_job,
    litecr.consumer_receipt,
    litecr.schema_meta,
    litecr.outbox_event,
    litecr.source_record,
    litecr.source_record_version,
    litecr.source_assertion,
    litecr.source_identifier_assertion,
    matching.normalized_assertion,
    matching.normalized_identifier,
    matching.indexed_source_version
TO litecr_worker;
GRANT UPDATE (state, attempts, available_at, lease_owner, lease_token,
              lease_expires_at, updated_at, last_error_code)
ON TABLE litecr.consumer_job TO litecr_worker;
GRANT INSERT ON TABLE litecr.consumer_receipt TO litecr_worker;
GRANT INSERT ON TABLE matching.evidence_run, matching.evidence_candidate TO litecr_worker;
-- INSERT ... RETURNING run_id needs SELECT on that returned column only.
GRANT SELECT (run_id) ON TABLE matching.evidence_run TO litecr_worker;
-- Historical schema-22/23/24 binaries persist their response after INSERT.
-- Schema-27 through 31 code inserts a complete snapshot and needs no rewrite grant.
DO $$ BEGIN
  IF EXISTS (SELECT 1 FROM litecr.schema_meta WHERE singleton AND version IN (22,23,24)) THEN
    GRANT UPDATE (result_snapshot) ON TABLE matching.evidence_run TO litecr_worker;
  ELSE
    REVOKE UPDATE (result_snapshot) ON TABLE matching.evidence_run FROM litecr_worker;
  END IF;
END $$;
GRANT EXECUTE ON FUNCTION matching.lock_advisory_source_snapshot(TEXT, TEXT, TEXT[]) TO litecr_worker;

DO $$
DECLARE
    unexpected_table text;
    legacy_snapshot_update boolean := (SELECT version IN (22,23,24)
        FROM litecr.schema_meta WHERE singleton);
BEGIN
    IF has_database_privilege('litecr_worker', current_database(), 'CREATE')
       OR has_schema_privilege('litecr_worker','public','CREATE')
       OR has_schema_privilege('litecr_worker','litecr','CREATE')
       OR has_schema_privilege('litecr_worker','matching','CREATE') THEN
        RAISE EXCEPTION 'WORKER_ROLE_VERIFY_DDL';
    END IF;
    IF has_table_privilege('litecr_worker','public._sqlx_migrations','INSERT')
       OR has_table_privilege('litecr_worker','public._sqlx_migrations','UPDATE')
       OR has_table_privilege('litecr_worker','public._sqlx_migrations','DELETE')
       OR has_table_privilege('litecr_worker','public._sqlx_migrations','TRUNCATE') THEN
        RAISE EXCEPTION 'WORKER_ROLE_VERIFY_SQLX_WRITE';
    END IF;
    IF NOT has_function_privilege('litecr_worker',
          'matching.lock_advisory_source_snapshot(text,text,text[])','EXECUTE')
       OR EXISTS (
          SELECT 1 FROM pg_proc p
          CROSS JOIN LATERAL aclexplode(COALESCE(p.proacl,acldefault('f',p.proowner))) acl
          WHERE p.oid='matching.lock_advisory_source_snapshot(text,text,text[])'::regprocedure
            AND acl.grantee=0 AND acl.privilege_type='EXECUTE'
       ) THEN
        RAISE EXCEPTION 'WORKER_ROLE_VERIFY_SOURCE_LOCK_EXECUTE';
    END IF;
    SELECT format('%I.%I (select=%s insert=%s update=%s delete=%s truncate=%s)',
                  n.nspname,c.relname,
                  has_table_privilege('litecr_worker',c.oid,'SELECT'),
                  has_table_privilege('litecr_worker',c.oid,'INSERT'),
                  has_table_privilege('litecr_worker',c.oid,'UPDATE'),
                  has_table_privilege('litecr_worker',c.oid,'DELETE'),
                  has_table_privilege('litecr_worker',c.oid,'TRUNCATE'))
      INTO unexpected_table
      FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace
      WHERE n.nspname IN ('litecr','matching') AND c.relkind IN ('r','p')
        AND ((has_table_privilege('litecr_worker',c.oid,'SELECT') AND NOT (
               (n.nspname='litecr' AND c.relname IN
                 ('consumer_job','consumer_receipt','schema_meta','outbox_event','source_record','source_record_version','source_assertion','source_identifier_assertion')) OR
               (n.nspname='matching' AND c.relname IN ('normalized_assertion','normalized_identifier','indexed_source_version'))))
          OR (n.nspname='matching' AND c.relname='evidence_run' AND
                has_column_privilege('litecr_worker',c.oid,'result_snapshot','SELECT'))
          OR (n.nspname='matching' AND c.relname='evidence_run' AND EXISTS (
                SELECT 1 FROM pg_attribute a WHERE a.attrelid=c.oid AND a.attnum>0
                  AND NOT a.attisdropped AND a.attname <> 'run_id'
                  AND has_column_privilege('litecr_worker',c.oid,a.attname,'SELECT')))
          OR (has_any_column_privilege('litecr_worker',c.oid,'UPDATE') AND NOT (
                (n.nspname='litecr' AND c.relname='consumer_job') OR
                (n.nspname='matching' AND c.relname='evidence_run')))
          OR (n.nspname='matching' AND c.relname='evidence_run' AND (
                has_table_privilege('litecr_worker',c.oid,'UPDATE') OR
                (has_column_privilege('litecr_worker',c.oid,'result_snapshot','UPDATE') <> legacy_snapshot_update) OR
                EXISTS (SELECT 1 FROM pg_attribute a WHERE a.attrelid=c.oid AND a.attnum>0
                        AND NOT a.attisdropped AND a.attname <> 'result_snapshot'
                        AND has_column_privilege('litecr_worker',c.oid,a.attname,'UPDATE'))))
          OR (has_table_privilege('litecr_worker',c.oid,'INSERT') AND NOT
               ((n.nspname='litecr' AND c.relname='consumer_receipt') OR
                (n.nspname='matching' AND c.relname IN ('evidence_run','evidence_candidate'))))
          OR has_table_privilege('litecr_worker',c.oid,'DELETE')
          OR has_table_privilege('litecr_worker',c.oid,'TRUNCATE'))
      LIMIT 1;
    IF unexpected_table IS NOT NULL THEN
      RAISE EXCEPTION 'WORKER_ROLE_VERIFY_UNEXPECTED_TABLE_PRIVILEGE:%',unexpected_table;
    END IF;
    IF NOT has_table_privilege('litecr_worker','litecr.consumer_job','SELECT')
       OR NOT has_table_privilege('litecr_worker','litecr.consumer_receipt','SELECT')
       OR NOT has_table_privilege('litecr_worker','litecr.schema_meta','SELECT')
       OR NOT has_table_privilege('litecr_worker','litecr.outbox_event','SELECT')
       OR NOT has_table_privilege('litecr_worker','litecr.source_record','SELECT')
       OR NOT has_table_privilege('litecr_worker','litecr.source_record_version','SELECT')
       OR NOT has_table_privilege('litecr_worker','litecr.source_assertion','SELECT')
       OR NOT has_table_privilege('litecr_worker','litecr.source_identifier_assertion','SELECT')
       OR NOT has_table_privilege('litecr_worker','matching.normalized_assertion','SELECT')
       OR NOT has_table_privilege('litecr_worker','matching.normalized_identifier','SELECT')
       OR NOT has_table_privilege('litecr_worker','matching.indexed_source_version','SELECT')
       OR has_table_privilege('litecr_worker','litecr.consumer_job','UPDATE')
       OR NOT has_column_privilege('litecr_worker','litecr.consumer_job','state','UPDATE')
       OR NOT has_column_privilege('litecr_worker','litecr.consumer_job','attempts','UPDATE')
       OR NOT has_column_privilege('litecr_worker','litecr.consumer_job','available_at','UPDATE')
       OR NOT has_column_privilege('litecr_worker','litecr.consumer_job','lease_owner','UPDATE')
       OR NOT has_column_privilege('litecr_worker','litecr.consumer_job','lease_token','UPDATE')
       OR NOT has_column_privilege('litecr_worker','litecr.consumer_job','lease_expires_at','UPDATE')
       OR NOT has_column_privilege('litecr_worker','litecr.consumer_job','updated_at','UPDATE')
       OR NOT has_column_privilege('litecr_worker','litecr.consumer_job','last_error_code','UPDATE')
       OR has_column_privilege('litecr_worker','litecr.consumer_job','consumer_name','UPDATE')
       OR has_column_privilege('litecr_worker','litecr.consumer_job','event_id','UPDATE')
       OR has_column_privilege('litecr_worker','litecr.consumer_job','registry_id','UPDATE')
       OR has_column_privilege('litecr_worker','litecr.consumer_job','organization_id','UPDATE')
       OR has_column_privilege('litecr_worker','litecr.consumer_job','source_record_id','UPDATE')
       OR has_column_privilege('litecr_worker','litecr.consumer_job','expected_receive_version','UPDATE')
       OR has_column_privilege('litecr_worker','litecr.consumer_job','created_at','UPDATE')
       OR NOT has_table_privilege('litecr_worker','litecr.consumer_receipt','INSERT')
       OR NOT has_table_privilege('litecr_worker','matching.evidence_run','INSERT')
       OR NOT has_table_privilege('litecr_worker','matching.evidence_candidate','INSERT')
        OR NOT has_column_privilege('litecr_worker','matching.evidence_run','run_id','SELECT')
        OR (has_column_privilege('litecr_worker','matching.evidence_run','result_snapshot','UPDATE') <> legacy_snapshot_update) THEN
      RAISE EXCEPTION 'WORKER_ROLE_VERIFY_REQUIRED_PRIVILEGE_MISSING';
    END IF;
    IF EXISTS (
      SELECT 1 FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace
      WHERE n.nspname IN ('litecr','matching') AND c.relkind IN ('r','p')
        AND ((has_any_column_privilege('litecr_worker',c.oid,'UPDATE') AND NOT (
               (n.nspname='litecr' AND c.relname='consumer_job') OR
               (n.nspname='matching' AND c.relname='evidence_run')))
          OR (n.nspname='matching' AND c.relname='evidence_run' AND (
               has_table_privilege('litecr_worker',c.oid,'UPDATE') OR
               (has_column_privilege('litecr_worker',c.oid,'result_snapshot','UPDATE') <> legacy_snapshot_update) OR
               EXISTS (SELECT 1 FROM pg_attribute a WHERE a.attrelid=c.oid AND a.attnum>0
                       AND NOT a.attisdropped AND a.attname <> 'result_snapshot'
                       AND has_column_privilege('litecr_worker',c.oid,a.attname,'UPDATE'))))
         OR (has_table_privilege('litecr_worker',c.oid,'INSERT') AND NOT
           ((n.nspname='litecr' AND c.relname='consumer_receipt') OR
            (n.nspname='matching' AND c.relname IN ('evidence_run','evidence_candidate'))))
         OR has_table_privilege('litecr_worker',c.oid,'DELETE')
         OR has_table_privilege('litecr_worker',c.oid,'TRUNCATE'))
    ) THEN
      RAISE EXCEPTION 'WORKER_ROLE_VERIFY_WRITE_SCOPE';
    END IF;
    IF EXISTS (
      SELECT 1 FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace
      WHERE n.nspname IN ('litecr','matching') AND c.relkind IN ('S','s')
        AND has_sequence_privilege('litecr_worker',c.oid,'USAGE')
    ) THEN
      RAISE EXCEPTION 'WORKER_ROLE_VERIFY_SEQUENCE_PRIVILEGE';
    END IF;
END $$;

\echo WORKER_ROLE_ACL_VERIFICATION_OK
