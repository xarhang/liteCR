-- DISPOSABLE LOCAL TEST FIXTURE ONLY. Never include in production bootstrap.
-- Apply after the liteCR access migration to the isolated local test database.
-- The Keycloak realm role is not authorization: this is the explicit liteCR grant.
BEGIN;

INSERT INTO litecr.access_identity (actor_kind, issuer_url, external_subject, is_active)
VALUES ('human', 'http://127.0.0.1:18081/realms/litecr', '00000000-0000-4000-8000-000000000042', TRUE)
ON CONFLICT (actor_kind, issuer_url, external_subject)
DO UPDATE SET is_active = TRUE, updated_at = now();

INSERT INTO litecr.access_grant (
    actor_kind, issuer_url, external_subject, registry_id, organization_id,
    role_name, permission_name, grant_source, granted_by
)
SELECT
    'human', 'http://127.0.0.1:18081/realms/litecr', '00000000-0000-4000-8000-000000000042',
    'fixture-registry', 'fixture-organization',
    'registrar', NULL, 'bootstrap', 'fixture-only-local-bootstrap'
WHERE NOT EXISTS (
    SELECT 1
    FROM litecr.access_grant
    WHERE actor_kind = 'human'
      AND issuer_url = 'http://127.0.0.1:18081/realms/litecr'
      AND external_subject = '00000000-0000-4000-8000-000000000042'
      AND registry_id = 'fixture-registry'
      AND organization_id = 'fixture-organization'
      AND role_name = 'registrar'
      AND revoked_at IS NULL
);

COMMIT;
