-- The OpenID Connect crate emits a 128-bit random nonce encoded as 22 base64url
-- characters. Accept that standards-compatible value while preserving the upper
-- bound and the database-only hash for flow state.
ALTER TABLE litecr.oidc_login_transaction
    DROP CONSTRAINT oidc_login_transaction_nonce_check;

ALTER TABLE litecr.oidc_login_transaction
    ADD CONSTRAINT oidc_login_transaction_nonce_check
    CHECK (nonce IS NULL OR length(nonce) BETWEEN 22 AND 256);

UPDATE litecr.schema_meta
SET version = 3,
    applied_at = now()
WHERE singleton = TRUE;
