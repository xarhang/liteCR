-- Run after `litecr migrate`, connected as the dedicated litecr_migrator role.
-- This script never creates roles, changes passwords, or changes database ownership.
-- A DBA must provision those objects separately and make the migration role the
-- owner of this dedicated database. Any different ownership model fails closed.
\set ON_ERROR_STOP on

DO $$
DECLARE
    db_owner name;
    runtime_role_oid oid;
    migration_role_oid oid;
BEGIN
    IF current_user <> 'litecr_migrator' THEN
        RAISE EXCEPTION 'ROLE_PREFLIGHT_MIGRATION_LOGIN';
    END IF;

    SELECT pg_get_userbyid(datdba)::name INTO db_owner
    FROM pg_database WHERE datname = current_database();
    IF db_owner IS DISTINCT FROM current_user THEN
        RAISE EXCEPTION 'ROLE_PREFLIGHT_DATABASE_OWNER';
    END IF;

    SELECT oid INTO migration_role_oid FROM pg_roles WHERE rolname = 'litecr_migrator'
      AND rolcanlogin AND NOT rolsuper AND NOT rolcreatedb AND NOT rolcreaterole
      AND NOT rolreplication AND NOT rolbypassrls;
    IF migration_role_oid IS NULL THEN
        RAISE EXCEPTION 'ROLE_PREFLIGHT_MIGRATOR_ATTRIBUTES';
    END IF;

    SELECT oid INTO runtime_role_oid FROM pg_roles WHERE rolname = 'litecr_runtime'
      AND rolcanlogin AND NOT rolsuper AND NOT rolcreatedb AND NOT rolcreaterole
      AND NOT rolreplication AND NOT rolbypassrls;
    IF runtime_role_oid IS NULL THEN
        RAISE EXCEPTION 'ROLE_PREFLIGHT_RUNTIME_ATTRIBUTES';
    END IF;

    IF EXISTS (SELECT 1 FROM pg_auth_members WHERE member = runtime_role_oid) THEN
        RAISE EXCEPTION 'ROLE_PREFLIGHT_RUNTIME_MEMBERSHIP';
    END IF;

    IF NOT EXISTS (SELECT 1 FROM pg_namespace WHERE nspname = 'litecr' AND nspowner = migration_role_oid)
       OR NOT EXISTS (SELECT 1 FROM pg_namespace WHERE nspname = 'matching' AND nspowner = migration_role_oid) THEN
        RAISE EXCEPTION 'ROLE_PREFLIGHT_SCHEMA_OWNER';
    END IF;

    IF to_regclass('public._sqlx_migrations') IS NULL THEN
        RAISE EXCEPTION 'ROLE_PREFLIGHT_SQLX_LEDGER_MISSING';
    END IF;
    IF (SELECT relowner FROM pg_class WHERE oid = 'public._sqlx_migrations'::regclass) <> migration_role_oid THEN
        RAISE EXCEPTION 'ROLE_PREFLIGHT_SQLX_LEDGER_OWNER';
    END IF;

    IF has_database_privilege('litecr_runtime', current_database(), 'CREATE')
       OR has_schema_privilege('litecr_runtime', 'public', 'CREATE')
       OR has_schema_privilege('litecr_runtime', 'litecr', 'CREATE')
       OR has_schema_privilege('litecr_runtime', 'matching', 'CREATE') THEN
        RAISE EXCEPTION 'ROLE_PREFLIGHT_RUNTIME_HAS_DDL';
    END IF;

    IF has_table_privilege('litecr_runtime', 'public._sqlx_migrations', 'INSERT')
       OR has_table_privilege('litecr_runtime', 'public._sqlx_migrations', 'UPDATE')
       OR has_table_privilege('litecr_runtime', 'public._sqlx_migrations', 'DELETE')
       OR has_table_privilege('litecr_runtime', 'public._sqlx_migrations', 'TRUNCATE') THEN
        RAISE EXCEPTION 'ROLE_PREFLIGHT_RUNTIME_CAN_WRITE_SQLX_LEDGER';
    END IF;
END $$;

DO $$
BEGIN
    EXECUTE format('GRANT CONNECT ON DATABASE %I TO litecr_runtime', current_database());
END $$;

GRANT USAGE ON SCHEMA litecr, matching TO litecr_runtime;
-- pg_dump --no-acl restores functions with PUBLIC EXECUTE defaults. Reapply
-- the migration's restriction on this worker-only SECURITY DEFINER boundary.
-- Preserve any separately provisioned worker grant.
REVOKE ALL ON FUNCTION matching.lock_advisory_source_snapshot(TEXT, TEXT, TEXT[])
    FROM PUBLIC, litecr_runtime;
GRANT SELECT, INSERT, UPDATE ON ALL TABLES IN SCHEMA litecr TO litecr_runtime;
GRANT SELECT, INSERT ON ALL TABLES IN SCHEMA matching TO litecr_runtime;
GRANT DELETE ON TABLE litecr.person_identifier TO litecr_runtime;
-- MCP credentials and their tool grants are provisioned/revoked through the
-- operator control plane, never through the API runtime DB role. This prevents
-- credential revival, token replacement, and post-issuance tool widening.
REVOKE INSERT, UPDATE, DELETE, TRUNCATE ON TABLE
    litecr.access_mcp_stdio_credential,
    litecr.access_mcp_stdio_credential_tool
    FROM litecr_runtime;
-- The API may enqueue through the accepted-source outbox trigger, but only the
-- dedicated worker may claim, retry, or complete a durable consumer job.
DO $$
BEGIN
    IF to_regclass('litecr.consumer_job') IS NOT NULL THEN
        EXECUTE 'REVOKE UPDATE ON TABLE litecr.consumer_job FROM litecr_runtime';
    END IF;
END $$;
REVOKE UPDATE, DELETE ON TABLE litecr.mutation_audit, litecr.outbox_event,
    litecr.configuration_audit, litecr.access_audit FROM litecr_runtime;
-- Per-tool MCP invocation evidence is append-only even for the runtime role.
-- PUBLIC is revoked by the migration; this also removes the broad grant above.
REVOKE UPDATE, DELETE, TRUNCATE ON TABLE litecr.mcp_invocation_audit FROM litecr_runtime;
GRANT USAGE, SELECT ON ALL SEQUENCES IN SCHEMA litecr, matching TO litecr_runtime;

ALTER DEFAULT PRIVILEGES FOR ROLE litecr_migrator IN SCHEMA litecr
    GRANT SELECT, INSERT ON TABLES TO litecr_runtime;
ALTER DEFAULT PRIVILEGES FOR ROLE litecr_migrator IN SCHEMA matching
    GRANT SELECT, INSERT ON TABLES TO litecr_runtime;
ALTER DEFAULT PRIVILEGES FOR ROLE litecr_migrator IN SCHEMA litecr
    GRANT USAGE, SELECT ON SEQUENCES TO litecr_runtime;
ALTER DEFAULT PRIVILEGES FOR ROLE litecr_migrator IN SCHEMA matching
    GRANT USAGE, SELECT ON SEQUENCES TO litecr_runtime;

-- The runtime's migration check is read-only. It must not be able to mutate the
-- SQLx ledger even though the release's `migrate` command uses the same table.
GRANT SELECT ON TABLE public._sqlx_migrations TO litecr_runtime;

DO $$
BEGIN
    IF has_database_privilege('litecr_runtime', current_database(), 'CREATE')
       OR has_schema_privilege('litecr_runtime', 'public', 'CREATE')
       OR has_schema_privilege('litecr_runtime', 'litecr', 'CREATE')
       OR has_schema_privilege('litecr_runtime', 'matching', 'CREATE') THEN
        RAISE EXCEPTION 'ROLE_VERIFY_RUNTIME_HAS_DDL';
    END IF;
    IF has_table_privilege('litecr_runtime', 'public._sqlx_migrations', 'INSERT')
       OR has_table_privilege('litecr_runtime', 'public._sqlx_migrations', 'UPDATE')
       OR has_table_privilege('litecr_runtime', 'public._sqlx_migrations', 'DELETE')
       OR has_table_privilege('litecr_runtime', 'public._sqlx_migrations', 'TRUNCATE') THEN
        RAISE EXCEPTION 'ROLE_VERIFY_RUNTIME_CAN_WRITE_SQLX_LEDGER';
    END IF;
    IF EXISTS (
        SELECT 1 FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
        WHERE n.nspname IN ('litecr', 'matching') AND c.relkind IN ('r', 'p')
          AND NOT has_table_privilege('litecr_runtime', c.oid, 'SELECT')
    ) THEN
        RAISE EXCEPTION 'ROLE_VERIFY_RUNTIME_TABLE_READ';
    END IF;
    IF EXISTS (
        SELECT 1 FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
        WHERE n.nspname IN ('litecr', 'matching') AND c.relkind IN ('r', 'p')
          AND NOT (n.nspname = 'litecr' AND c.relname IN (
              'access_mcp_stdio_credential', 'access_mcp_stdio_credential_tool'
          ))
          AND NOT has_table_privilege('litecr_runtime', c.oid, 'INSERT')
    ) THEN
        RAISE EXCEPTION 'ROLE_VERIFY_RUNTIME_TABLE_INSERT';
    END IF;
    IF EXISTS (
        SELECT 1 FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
        WHERE n.nspname = 'litecr' AND c.relkind IN ('r', 'p')
          AND c.relname NOT IN (
              'mutation_audit', 'outbox_event', 'configuration_audit', 'access_audit',
              'mcp_invocation_audit', 'access_mcp_stdio_credential',
              'access_mcp_stdio_credential_tool', 'consumer_job'
          )
          AND NOT has_table_privilege('litecr_runtime', c.oid, 'UPDATE')
    ) OR EXISTS (
        SELECT 1 FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
        WHERE n.nspname = 'matching' AND c.relkind IN ('r', 'p')
          AND has_table_privilege('litecr_runtime', c.oid, 'UPDATE')
    ) THEN
        RAISE EXCEPTION 'ROLE_VERIFY_RUNTIME_TABLE_UPDATE';
    END IF;
    IF EXISTS (
        SELECT 1 FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
        WHERE n.nspname IN ('litecr', 'matching') AND c.relkind IN ('r', 'p')
          AND c.relname <> 'person_identifier' AND has_table_privilege('litecr_runtime', c.oid, 'DELETE')
    ) OR NOT has_table_privilege('litecr_runtime', 'litecr.person_identifier', 'DELETE') THEN
        RAISE EXCEPTION 'ROLE_VERIFY_RUNTIME_TABLE_DELETE';
    END IF;
    IF has_table_privilege('litecr_runtime', 'litecr.mutation_audit', 'UPDATE')
       OR has_table_privilege('litecr_runtime', 'litecr.outbox_event', 'UPDATE')
       OR has_table_privilege('litecr_runtime', 'litecr.configuration_audit', 'UPDATE')
       OR has_table_privilege('litecr_runtime', 'litecr.access_audit', 'UPDATE')
       OR has_table_privilege('litecr_runtime', 'litecr.mcp_invocation_audit', 'UPDATE')
       OR has_table_privilege('litecr_runtime', 'matching.evidence_run', 'UPDATE')
       OR has_table_privilege('litecr_runtime', 'matching.evidence_candidate', 'UPDATE') THEN
        RAISE EXCEPTION 'ROLE_VERIFY_APPEND_ONLY_TABLE_UPDATE';
    END IF;
    IF has_table_privilege('litecr_runtime', 'litecr.mcp_invocation_audit', 'DELETE')
       OR has_table_privilege('litecr_runtime', 'litecr.mcp_invocation_audit', 'TRUNCATE') THEN
        RAISE EXCEPTION 'ROLE_VERIFY_MCP_AUDIT_MUTATION';
    END IF;
    IF has_table_privilege('litecr_runtime', 'litecr.access_mcp_stdio_credential', 'INSERT')
       OR has_table_privilege('litecr_runtime', 'litecr.access_mcp_stdio_credential', 'UPDATE')
       OR has_table_privilege('litecr_runtime', 'litecr.access_mcp_stdio_credential', 'DELETE')
       OR has_table_privilege('litecr_runtime', 'litecr.access_mcp_stdio_credential', 'TRUNCATE')
       OR has_table_privilege('litecr_runtime', 'litecr.access_mcp_stdio_credential_tool', 'INSERT')
       OR has_table_privilege('litecr_runtime', 'litecr.access_mcp_stdio_credential_tool', 'UPDATE')
       OR has_table_privilege('litecr_runtime', 'litecr.access_mcp_stdio_credential_tool', 'DELETE')
       OR has_table_privilege('litecr_runtime', 'litecr.access_mcp_stdio_credential_tool', 'TRUNCATE') THEN
        RAISE EXCEPTION 'ROLE_VERIFY_RUNTIME_MCP_CREDENTIAL_MUTATION';
    END IF;
    IF to_regclass('litecr.consumer_job') IS NOT NULL
       AND has_table_privilege('litecr_runtime', 'litecr.consumer_job', 'UPDATE') THEN
        RAISE EXCEPTION 'ROLE_VERIFY_RUNTIME_CAN_UPDATE_CONSUMER_JOB';
    END IF;
    IF has_function_privilege('litecr_runtime',
        'matching.lock_advisory_source_snapshot(text,text,text[])','EXECUTE') THEN
        RAISE EXCEPTION 'ROLE_VERIFY_RUNTIME_CAN_EXECUTE_WORKER_SOURCE_LOCK';
    END IF;
END $$;

\echo ROLE_ACL_VERIFICATION_OK
