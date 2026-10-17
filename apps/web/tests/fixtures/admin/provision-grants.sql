-- Synthetic local Phase 4 browser fixture only. Run against a newly-created,
-- disposable liteCR database after migrations; never use production databases.
\set ON_ERROR_STOP on
BEGIN;
SELECT set_config('litecr_fixture.run_id', :'run_id', true);
SELECT set_config('litecr_fixture.translator_subject', :'translator_subject', true);
SELECT set_config('litecr_fixture.reviewer_subject', :'reviewer_subject', true);
SELECT set_config('litecr_fixture.publisher_subject', :'publisher_subject', true);
SELECT set_config('litecr_fixture.administrator_subject', :'administrator_subject', true);
SELECT set_config('litecr_fixture.registrar_subject', :'registrar_subject', true);

DO $$
DECLARE
    run_id TEXT := current_setting('litecr_fixture.run_id');
    subjects TEXT[] := ARRAY[
        current_setting('litecr_fixture.translator_subject'),
        current_setting('litecr_fixture.reviewer_subject'),
        current_setting('litecr_fixture.publisher_subject'),
        current_setting('litecr_fixture.administrator_subject'),
        current_setting('litecr_fixture.registrar_subject')
    ];
BEGIN
    IF run_id !~ '^[a-f0-9]{16}$' THEN
        RAISE EXCEPTION 'admin E2E run_id must be 16 lowercase hexadecimal characters';
    END IF;
    IF EXISTS (SELECT 1 FROM unnest(subjects) AS subject WHERE subject !~ '^[0-9a-fA-F-]{36}$') THEN
        RAISE EXCEPTION 'each E2E actor must have a UUID OIDC subject';
    END IF;
    IF (SELECT count(DISTINCT subject) FROM unnest(subjects) AS subject) <> 5 THEN
        RAISE EXCEPTION 'E2E actors must be five distinct OIDC identities';
    END IF;
END $$;

INSERT INTO litecr.access_identity (actor_kind, issuer_url, external_subject)
SELECT 'human', 'http://127.0.0.1:18081/realms/litecr', subject
FROM unnest(ARRAY[
    :'translator_subject', :'reviewer_subject', :'publisher_subject', :'administrator_subject', :'registrar_subject'
]) AS subject;

-- IDs are deterministic from run_id, so the runner and UI cannot disagree.
INSERT INTO litecr.access_grant (
    actor_kind, issuer_url, external_subject, registry_id, organization_id,
    role_name, permission_name, grant_source, granted_by
)
SELECT 'human', 'http://127.0.0.1:18081/realms/litecr', :'translator_subject',
       'litecr-admin-e2e-' || :'run_id', 'litecr-admin-org-' || :'run_id',
       'translator', NULL, 'bootstrap', 'synthetic-phase4-fixture';

INSERT INTO litecr.access_grant (
    actor_kind, issuer_url, external_subject, registry_id, organization_id,
    role_name, permission_name, grant_source, granted_by
)
SELECT 'human', 'http://127.0.0.1:18081/realms/litecr', :'reviewer_subject',
       'litecr-admin-e2e-' || :'run_id', 'litecr-admin-org-' || :'run_id',
       'reviewer', NULL, 'bootstrap', 'synthetic-phase4-fixture';
INSERT INTO litecr.access_grant (
    actor_kind, issuer_url, external_subject, registry_id, organization_id,
    role_name, permission_name, grant_source, granted_by
)
SELECT 'human', 'http://127.0.0.1:18081/realms/litecr', :'reviewer_subject',
       'litecr-admin-e2e-' || :'run_id', 'litecr-admin-org-' || :'run_id',
       NULL, 'match:request', 'bootstrap', 'synthetic-review-e2e-fixture';
-- The registration-to-review browser flow needs exact public-ID lookup. Keep
-- this least-privilege exception run-scoped instead of widening the reviewer role.
INSERT INTO litecr.access_grant (
    actor_kind, issuer_url, external_subject, registry_id, organization_id,
    role_name, permission_name, grant_source, granted_by
)
SELECT 'human', 'http://127.0.0.1:18081/realms/litecr', :'reviewer_subject',
       'litecr-admin-e2e-' || :'run_id', 'litecr-admin-org-' || :'run_id',
       NULL, 'person:search', 'bootstrap', 'synthetic-registry-search-e2e-fixture';
INSERT INTO litecr.access_grant (
    actor_kind, issuer_url, external_subject, registry_id, organization_id,
    role_name, permission_name, grant_source, granted_by
)
SELECT 'human', 'http://127.0.0.1:18081/realms/litecr', :'reviewer_subject',
       'litecr-admin-e2e-' || :'run_id', 'litecr-admin-org-' || :'run_id',
       NULL, 'translation:review', 'bootstrap', 'synthetic-phase4-fixture';
INSERT INTO litecr.access_grant (
    actor_kind, issuer_url, external_subject, registry_id, organization_id,
    role_name, permission_name, grant_source, granted_by
)
SELECT 'human', 'http://127.0.0.1:18081/realms/litecr', :'reviewer_subject',
       'litecr-admin-e2e-' || :'run_id', 'litecr-admin-org-' || :'run_id',
       NULL, 'translation:draft:read', 'bootstrap', 'synthetic-phase4-fixture';

INSERT INTO litecr.access_grant (
    actor_kind, issuer_url, external_subject, registry_id, organization_id,
    role_name, permission_name, grant_source, granted_by
)
SELECT 'human', 'http://127.0.0.1:18081/realms/litecr', :'publisher_subject',
       'litecr-admin-e2e-' || :'run_id', 'litecr-admin-org-' || :'run_id',
       NULL, 'translation:publish', 'bootstrap', 'synthetic-phase4-fixture';
INSERT INTO litecr.access_grant (
    actor_kind, issuer_url, external_subject, registry_id, organization_id,
    role_name, permission_name, grant_source, granted_by
)
SELECT 'human', 'http://127.0.0.1:18081/realms/litecr', :'publisher_subject',
       'litecr-admin-e2e-' || :'run_id', 'litecr-admin-org-' || :'run_id',
       NULL, 'translation:draft:read', 'bootstrap', 'synthetic-phase4-fixture';

INSERT INTO litecr.access_grant (
    actor_kind, issuer_url, external_subject, registry_id, organization_id,
    role_name, permission_name, grant_source, granted_by
)
SELECT 'human', 'http://127.0.0.1:18081/realms/litecr', :'administrator_subject',
       'litecr-admin-e2e-' || :'run_id', 'litecr-admin-org-' || :'run_id',
       'administrator', NULL, 'bootstrap', 'synthetic-phase4-fixture';

INSERT INTO litecr.access_grant (
    actor_kind, issuer_url, external_subject, registry_id, organization_id,
    role_name, permission_name, grant_source, granted_by
)
SELECT 'human', 'http://127.0.0.1:18081/realms/litecr', :'registrar_subject',
       'litecr-admin-e2e-' || :'run_id', 'litecr-admin-org-' || :'run_id',
       'registrar', NULL, 'bootstrap', 'synthetic-review-e2e-fixture';

COMMIT;
