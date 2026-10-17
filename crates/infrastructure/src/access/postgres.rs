use async_trait::async_trait;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use litecr_application::access::{
    AccessAuthenticator, AccessDirectory, AccessDirectoryError, AccessGrant, AccessIdentity,
    AccessSecurityAudit, AccessSession, AccessSessionRepository, ActorKind, ActorRef,
    McpCredentialBinding, McpCredentialIdentity, McpCredentialProfile, McpCredentialStore,
    McpInvocationAudit, McpInvocationAuditError, McpInvocationAuditEvent, OidcAuthorization,
    OidcFlow, OidcFlowSecrets, OidcFlowStore, OidcProvider, OidcProviderError, Permission, Role,
    SecurityAuditError, SecurityAuditEvent, SessionCredentials, SessionError,
};
use openidconnect::{
    AuthorizationCode, ClientId, ClientSecret, CsrfToken, IssuerUrl, Nonce, PkceCodeChallenge,
    PkceCodeVerifier, RedirectUrl, Scope, TokenResponse,
    core::{CoreAuthenticationFlow, CoreClient, CoreProviderMetadata},
};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};
use subtle::ConstantTimeEq;

fn actor_kind_name(kind: ActorKind) -> &'static str {
    match kind {
        ActorKind::Human => "human",
        ActorKind::IntegrationClient => "integration_client",
        ActorKind::System => "system",
    }
}

fn parse_actor_kind(kind: &str) -> Result<ActorKind, AccessDirectoryError> {
    match kind {
        "human" => Ok(ActorKind::Human),
        "integration_client" => Ok(ActorKind::IntegrationClient),
        "system" => Ok(ActorKind::System),
        _ => Err(AccessDirectoryError::Unavailable),
    }
}

fn digest_secret(value: &str) -> [u8; 32] {
    Sha256::digest(value.as_bytes()).into()
}

/// Call only after openidconnect has verified signature, issuer, audience, expiry, and nonce.
/// This unverified decode can only reject a verified token; it never supplies trusted claims.
fn verified_token_nbf_is_current_or_absent(token: &str, now_epoch_seconds: i64) -> bool {
    let Some(payload_segment) = token.split('.').nth(1) else {
        return false;
    };
    let Ok(payload) = URL_SAFE_NO_PAD.decode(payload_segment) else {
        return false;
    };
    let Ok(payload) = serde_json::from_slice::<serde_json::Value>(&payload) else {
        return false;
    };
    match payload.get("nbf") {
        None => true,
        Some(value) => value
            .as_i64()
            .is_some_and(|not_before| not_before <= now_epoch_seconds),
    }
}

fn audience_and_authorized_party_are_valid(
    audiences: &[String],
    authorized_party: Option<&str>,
    expected_client_id: &str,
) -> bool {
    audiences.len() == 1
        && audiences[0] == expected_client_id
        && authorized_party.is_none_or(|party| party == expected_client_id)
}

fn verified_issuer_and_audience_are_valid(
    issuer: &str,
    expected_issuer: &str,
    audiences: &[String],
    authorized_party: Option<&str>,
    expected_client_id: &str,
) -> bool {
    issuer == expected_issuer
        && audience_and_authorized_party_are_valid(audiences, authorized_party, expected_client_id)
}

fn issuer_transport_is_allowed(
    scheme: &str,
    host: Option<&str>,
    allow_http_loopback: bool,
) -> bool {
    scheme == "https"
        || (allow_http_loopback
            && scheme == "http"
            && matches!(host, Some("localhost" | "127.0.0.1" | "::1")))
}

fn map_grant(row: &sqlx::postgres::PgRow) -> Result<AccessGrant, AccessDirectoryError> {
    let role: Option<String> = row
        .try_get("role_name")
        .map_err(|_| AccessDirectoryError::Unavailable)?;
    let permission: Option<String> = row
        .try_get("permission_name")
        .map_err(|_| AccessDirectoryError::Unavailable)?;
    let roles = role
        .map(|name| Role::parse(&name).ok_or(AccessDirectoryError::Unavailable))
        .transpose()?
        .into_iter()
        .collect();
    let permissions = permission
        .map(|name| Permission::parse(&name).ok_or(AccessDirectoryError::Unavailable))
        .transpose()?
        .into_iter()
        .collect();
    Ok(AccessGrant {
        registry_id: row
            .try_get("registry_id")
            .map_err(|_| AccessDirectoryError::Unavailable)?,
        organization_id: row
            .try_get("organization_id")
            .map_err(|_| AccessDirectoryError::Unavailable)?,
        record_ids: row
            .try_get("record_ids")
            .map_err(|_| AccessDirectoryError::Unavailable)?,
        roles,
        permissions,
    })
}

#[derive(Clone)]
pub struct PostgresAccessDirectory {
    pool: PgPool,
}

impl PostgresAccessDirectory {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl AccessDirectory for PostgresAccessDirectory {
    async fn load_identity(
        &self,
        kind: ActorKind,
        issuer_url: Option<&str>,
        external_subject: &str,
    ) -> Result<Option<AccessIdentity>, AccessDirectoryError> {
        let active = sqlx::query_scalar::<_, bool>(
            "SELECT is_active FROM litecr.access_identity \
             WHERE actor_kind = $1 AND issuer_url = $2 AND external_subject = $3",
        )
        .bind(actor_kind_name(kind))
        .bind(issuer_url.unwrap_or_default())
        .bind(external_subject)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| AccessDirectoryError::Unavailable)?;

        let Some(active) = active else {
            return Ok(None);
        };
        if !active {
            return Err(AccessDirectoryError::Inactive);
        }

        let rows = sqlx::query(
            "SELECT registry_id, organization_id, record_ids, role_name, permission_name \
             FROM litecr.access_grant \
             WHERE actor_kind = $1 AND issuer_url = $2 AND external_subject = $3 AND revoked_at IS NULL \
             ORDER BY grant_id",
        )
        .bind(actor_kind_name(kind))
        .bind(issuer_url.unwrap_or_default())
        .bind(external_subject)
        .fetch_all(&self.pool)
        .await
        .map_err(|_| AccessDirectoryError::Unavailable)?;
        let grants = rows.iter().map(map_grant).collect::<Result<Vec<_>, _>>()?;

        Ok(Some(AccessIdentity {
            actor: ActorRef {
                kind,
                id: external_subject.to_owned(),
                issuer_url: issuer_url.map(str::to_owned),
            },
            active,
            grants,
        }))
    }
}

#[derive(Clone)]
pub struct PostgresAccessSecurityAudit {
    pool: PgPool,
}

impl PostgresAccessSecurityAudit {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl AccessSecurityAudit for PostgresAccessSecurityAudit {
    async fn append(&self, event: &SecurityAuditEvent) -> Result<(), SecurityAuditError> {
        if !event.validate() {
            return Err(SecurityAuditError);
        }
        sqlx::query(
            "WITH resolved_actor AS (\
                 SELECT identity_key, actor_kind FROM litecr.access_identity \
                 WHERE actor_kind = $5 AND issuer_url = $6 AND external_subject = $7\
             ) \
             INSERT INTO litecr.access_security_audit \
             (event_type, reason_code, route_family, scope_shape, actor_kind, actor_identity_key, request_id) \
             SELECT $1, $2, $3, $4, resolved_actor.actor_kind, resolved_actor.identity_key, $8 \
             FROM (SELECT 1) AS event LEFT JOIN resolved_actor ON TRUE",
        )
        .bind(event.event_type.as_str())
        .bind(event.reason.as_str())
        .bind(event.route_family.as_str())
        .bind(event.scope_shape.as_str())
        .bind(
            event
                .actor
                .as_ref()
                .map(|actor| actor_kind_name(actor.kind)),
        )
        .bind(
            event
                .actor
                .as_ref()
                .and_then(|actor| actor.issuer_url.as_deref())
                .unwrap_or_default(),
        )
        .bind(event.actor.as_ref().map(|actor| actor.id.as_str()))
        .bind(&event.request_id)
        .execute(&self.pool)
        .await
        .map_err(|_| SecurityAuditError)?;
        Ok(())
    }
}

#[derive(Clone)]
pub struct PostgresAccessSessionRepository {
    pool: PgPool,
    directory: PostgresAccessDirectory,
    idle_timeout_seconds: i64,
}

impl PostgresAccessSessionRepository {
    pub fn new(pool: PgPool, directory: PostgresAccessDirectory) -> Self {
        Self {
            pool,
            directory,
            idle_timeout_seconds: 900,
        }
    }

    /// Server-enforced inactivity bound, independent of absolute expiry.
    pub fn with_idle_timeout_seconds(mut self, seconds: i64) -> Option<Self> {
        if !(60..=1800).contains(&seconds) {
            return None;
        }
        self.idle_timeout_seconds = seconds;
        Some(self)
    }

    /// Creates an opaque one-time credential for a preprovisioned integration actor.
    /// The caller must protect the returned value as a secret and show it only once.
    pub async fn issue_integration_credential(
        &self,
        issuer_url: Option<&str>,
        external_subject: &str,
        expires_in_seconds: Option<i64>,
    ) -> Result<String, SessionError> {
        if expires_in_seconds.is_some_and(|seconds| seconds <= 0 || seconds > 31_536_000) {
            return Err(SessionError::Invalid);
        }
        let identity = self
            .directory
            .load_identity(ActorKind::IntegrationClient, issuer_url, external_subject)
            .await
            .map_err(|_| SessionError::Unavailable)?
            .filter(|identity| {
                identity.active
                    && !identity.grants.is_empty()
                    && identity.grants.iter().all(|grant| {
                        grant
                            .roles
                            .iter()
                            .all(|role| *role == Role::IntegrationClient)
                    })
            })
            .ok_or(SessionError::Invalid)?;
        let _ = identity;
        let credential = openidconnect::CsrfToken::new_random_len(32)
            .secret()
            .to_owned();
        sqlx::query(
            "INSERT INTO litecr.access_integration_credential \
             (token_hash, issuer_url, external_subject, expires_at) \
             VALUES ($1, $2, $3, CASE WHEN $4::bigint IS NULL THEN NULL \
                 ELSE now() + ($4 * interval '1 second') END)",
        )
        .bind(digest_secret(&credential).to_vec())
        .bind(issuer_url.unwrap_or_default())
        .bind(external_subject)
        .bind(expires_in_seconds)
        .execute(&self.pool)
        .await
        .map_err(|_| SessionError::Unavailable)?;
        Ok(credential)
    }

    pub async fn revoke_integration_credential(
        &self,
        opaque_credential: &str,
        request_id: &str,
    ) -> Result<bool, SessionError> {
        if opaque_credential.len() < 32 || opaque_credential.len() > 256 {
            return Ok(false);
        }
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| SessionError::Unavailable)?;
        let row = sqlx::query(
            "UPDATE litecr.access_integration_credential \
             SET revoked_at = COALESCE(revoked_at, now()) \
             WHERE token_hash = $1 AND revoked_at IS NULL \
             RETURNING issuer_url, external_subject",
        )
        .bind(digest_secret(opaque_credential).to_vec())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|_| SessionError::Unavailable)?;
        let Some(row) = row else {
            transaction
                .commit()
                .await
                .map_err(|_| SessionError::Unavailable)?;
            return Ok(false);
        };
        let issuer: String = row
            .try_get("issuer_url")
            .map_err(|_| SessionError::Unavailable)?;
        let subject: String = row
            .try_get("external_subject")
            .map_err(|_| SessionError::Unavailable)?;
        sqlx::query(
            "INSERT INTO litecr.access_audit \
             (actor_kind, actor_issuer_url, actor_id, action, outcome, reason_code, request_id) \
             VALUES ('integration_client', $1, $2, 'integration.credential_revoked', 'revoked', 'credential_revoked', $3)",
        )
        .bind(issuer)
        .bind(subject)
        .bind(request_id)
        .execute(&mut *transaction)
        .await
        .map_err(|_| SessionError::Unavailable)?;
        transaction
            .commit()
            .await
            .map_err(|_| SessionError::Unavailable)?;
        Ok(true)
    }
}

#[async_trait]
impl McpCredentialStore for PostgresAccessSessionRepository {
    async fn issue(
        &self,
        actor: &ActorRef,
        profile: &McpCredentialProfile,
        request_id: &str,
    ) -> Result<String, SessionError> {
        if actor.kind != ActorKind::IntegrationClient
            || profile.ttl_seconds <= 0
            || profile.ttl_seconds > litecr_application::access::MCP_CREDENTIAL_MAX_TTL_SECONDS
            || profile.allowed_tools.is_empty()
            || !profile
                .allowed_tools
                .iter()
                .all(|tool| litecr_application::access::McpToolId::parse(tool.as_str()).is_some())
            || request_id.is_empty()
            || request_id.len() > 128
            || !request_id.bytes().all(|byte| byte.is_ascii_graphic())
        {
            return Err(SessionError::Invalid);
        }
        let identity =
            self.directory
                .load_identity(actor.kind, actor.issuer_url.as_deref(), &actor.id)
                .await
                .map_err(|_| SessionError::Unavailable)?
                .filter(|identity| {
                    identity.active
                        && !identity.grants.is_empty()
                        && identity.grants.iter().all(|grant| {
                            grant
                                .roles
                                .iter()
                                .all(|role| *role == Role::IntegrationClient)
                        })
                        && profile.allowed_tools.iter().all(|tool| {
                            identity.grants.iter().any(|grant| {
                                grant.permissions.contains(&tool.required_permission())
                            })
                        })
                })
                .ok_or(SessionError::Invalid)?;
        let _ = identity;

        let opaque_credential = CsrfToken::new_random_len(32).secret().to_owned();
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| SessionError::Unavailable)?;
        let credential_id: i64 = sqlx::query_scalar(
            "INSERT INTO litecr.access_mcp_stdio_credential \
             (token_hash, actor_kind, issuer_url, external_subject, audience, client_id, purpose_code, expires_at) \
             VALUES ($1, 'integration_client', $2, $3, $4, $5, $6, now() + ($7 * interval '1 second')) \
             RETURNING credential_id",
        )
        .bind(digest_secret(&opaque_credential).to_vec())
        .bind(actor.issuer_url.as_deref().unwrap_or_default())
        .bind(&actor.id)
        .bind(litecr_application::access::MCP_STDIO_AUDIENCE)
        .bind(&profile.client_id)
        .bind(&profile.purpose_code)
        .bind(profile.ttl_seconds)
        .fetch_one(&mut *transaction)
        .await
        .map_err(|_| SessionError::Unavailable)?;
        for tool in &profile.allowed_tools {
            sqlx::query(
                "INSERT INTO litecr.access_mcp_stdio_credential_tool (credential_id, tool_id) VALUES ($1, $2)",
            )
            .bind(credential_id)
            .bind(tool.as_str())
            .execute(&mut *transaction)
            .await
            .map_err(|_| SessionError::Unavailable)?;
        }
        sqlx::query(
            "INSERT INTO litecr.access_audit \
             (actor_kind, actor_issuer_url, actor_id, action, resource_type, resource_ref, outcome, request_id) \
             VALUES ('integration_client', $1, $2, 'mcp.credential.issued', 'mcp_stdio_credential', $3, 'allowed', $4)",
        )
        .bind(actor.issuer_url.as_deref().unwrap_or_default())
        .bind(&actor.id)
        .bind(credential_id.to_string())
        .bind(request_id)
        .execute(&mut *transaction)
        .await
        .map_err(|_| SessionError::Unavailable)?;
        transaction
            .commit()
            .await
            .map_err(|_| SessionError::Unavailable)?;
        Ok(opaque_credential)
    }

    async fn authenticate(
        &self,
        opaque_credential: &str,
        _request_id: &str,
    ) -> Result<McpCredentialIdentity, SessionError> {
        if opaque_credential.len() < 32 || opaque_credential.len() > 256 {
            return Err(SessionError::Invalid);
        }
        let row = sqlx::query(
            "SELECT actor_kind, issuer_url, external_subject, client_id, purpose_code \
             FROM litecr.access_mcp_stdio_credential \
             WHERE token_hash = $1 AND audience = $2 AND revoked_at IS NULL AND expires_at > now() \
             LIMIT 1",
        )
        .bind(digest_secret(opaque_credential).to_vec())
        .bind(litecr_application::access::MCP_STDIO_AUDIENCE)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| SessionError::Unavailable)?
        .ok_or(SessionError::Invalid)?;
        let actor_kind = row
            .try_get::<String, _>("actor_kind")
            .map_err(|_| SessionError::Unavailable)?;
        if actor_kind != "integration_client" {
            return Err(SessionError::Invalid);
        }
        let issuer_url: String = row
            .try_get("issuer_url")
            .map_err(|_| SessionError::Unavailable)?;
        let external_subject: String = row
            .try_get("external_subject")
            .map_err(|_| SessionError::Unavailable)?;
        let client_id: String = row
            .try_get("client_id")
            .map_err(|_| SessionError::Unavailable)?;
        let purpose_code: String = row
            .try_get("purpose_code")
            .map_err(|_| SessionError::Unavailable)?;
        let identity = self
            .directory
            .load_identity(
                ActorKind::IntegrationClient,
                (!issuer_url.is_empty()).then_some(issuer_url.as_str()),
                &external_subject,
            )
            .await
            .map_err(|_| SessionError::Unavailable)?
            .filter(|identity| {
                identity.active
                    && !identity.grants.is_empty()
                    && identity.grants.iter().all(|grant| {
                        grant
                            .roles
                            .iter()
                            .all(|role| *role == Role::IntegrationClient)
                    })
            })
            .ok_or(SessionError::Invalid)?;
        let tool_ids: Vec<String> = sqlx::query_scalar(
            "SELECT tool_id FROM litecr.access_mcp_stdio_credential_tool \
             WHERE credential_id = (SELECT credential_id FROM litecr.access_mcp_stdio_credential WHERE token_hash = $1) \
             ORDER BY tool_id",
        )
        .bind(digest_secret(opaque_credential).to_vec())
        .fetch_all(&self.pool)
        .await
        .map_err(|_| SessionError::Unavailable)?;
        let allowed_tools = tool_ids
            .iter()
            .map(|tool_id| {
                litecr_application::access::McpToolId::parse(tool_id)
                    .ok_or(SessionError::Unavailable)
            })
            .collect::<Result<std::collections::BTreeSet<_>, _>>()?;
        if allowed_tools.is_empty() {
            return Err(SessionError::Invalid);
        }
        Ok(McpCredentialIdentity {
            identity,
            binding: McpCredentialBinding {
                client_id,
                purpose_code,
                allowed_tools,
            },
        })
    }

    async fn revoke(
        &self,
        opaque_credential: &str,
        request_id: &str,
    ) -> Result<bool, SessionError> {
        if opaque_credential.len() < 32 || opaque_credential.len() > 256 {
            return Ok(false);
        }
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| SessionError::Unavailable)?;
        let row = sqlx::query(
            "UPDATE litecr.access_mcp_stdio_credential \
             SET revoked_at = COALESCE(revoked_at, now()) \
             WHERE token_hash = $1 AND revoked_at IS NULL \
             RETURNING issuer_url, external_subject, credential_id",
        )
        .bind(digest_secret(opaque_credential).to_vec())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|_| SessionError::Unavailable)?;
        let Some(row) = row else {
            transaction
                .commit()
                .await
                .map_err(|_| SessionError::Unavailable)?;
            return Ok(false);
        };
        let issuer_url: String = row
            .try_get("issuer_url")
            .map_err(|_| SessionError::Unavailable)?;
        let external_subject: String = row
            .try_get("external_subject")
            .map_err(|_| SessionError::Unavailable)?;
        let credential_id: i64 = row
            .try_get("credential_id")
            .map_err(|_| SessionError::Unavailable)?;
        sqlx::query(
            "INSERT INTO litecr.access_audit \
             (actor_kind, actor_issuer_url, actor_id, action, resource_type, resource_ref, outcome, reason_code, request_id) \
             VALUES ('integration_client', $1, $2, 'mcp.credential.revoked', 'mcp_stdio_credential', $3, 'revoked', 'credential_revoked', $4)",
        )
        .bind(issuer_url)
        .bind(external_subject)
        .bind(credential_id.to_string())
        .bind(request_id)
        .execute(&mut *transaction)
        .await
        .map_err(|_| SessionError::Unavailable)?;
        transaction
            .commit()
            .await
            .map_err(|_| SessionError::Unavailable)?;
        Ok(true)
    }
}

#[derive(Clone)]
pub struct PostgresMcpInvocationAudit {
    pool: PgPool,
}

impl PostgresMcpInvocationAudit {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl McpInvocationAudit for PostgresMcpInvocationAudit {
    async fn append(&self, event: &McpInvocationAuditEvent) -> Result<(), McpInvocationAuditError> {
        if !event.validate() {
            return Err(McpInvocationAuditError);
        }
        let actor_identity_key = if let Some(actor) = event.actor() {
            Some(sqlx::query_scalar::<_, i64>(
                "SELECT identity_key FROM litecr.access_identity \
                 WHERE actor_kind = 'integration_client' AND issuer_url = $1 AND external_subject = $2",
            )
            .bind(actor.issuer_url.as_deref().unwrap_or_default())
            .bind(&actor.id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|_| McpInvocationAuditError)?
            .ok_or(McpInvocationAuditError)?)
        } else {
            None
        };
        sqlx::query(
            "INSERT INTO litecr.mcp_invocation_audit \
             (request_id, client_id, purpose_code, actor_identity_key, tool_id, registry_id, organization_id, outcome, reason_code, result_count) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
        )
        .bind(event.request_id())
        .bind(event.client_id())
        .bind(event.purpose_code())
        .bind(actor_identity_key)
        .bind(event.tool_id().as_str())
        .bind(event.registry_id())
        .bind(event.organization_id())
        .bind(event.outcome().as_str())
        .bind(event.reason().as_str())
        .bind(i32::from(event.result_count()))
        .execute(&self.pool)
        .await
        .map_err(|_| McpInvocationAuditError)?;
        Ok(())
    }
}

#[async_trait]
impl AccessSessionRepository for PostgresAccessSessionRepository {
    async fn create(
        &self,
        actor: &ActorRef,
        oidc_session_id: Option<&str>,
        expires_in_seconds: i64,
        request_id: &str,
    ) -> Result<SessionCredentials, SessionError> {
        if actor.kind != ActorKind::Human || expires_in_seconds <= 0 || expires_in_seconds > 86_400
        {
            return Err(SessionError::Invalid);
        }
        let identity = self
            .directory
            .load_identity(actor.kind, actor.issuer_url.as_deref(), &actor.id)
            .await
            .map_err(|_| SessionError::Unavailable)?
            .ok_or(SessionError::Invalid)?;
        if !identity.active || identity.grants.is_empty() {
            return Err(SessionError::Invalid);
        }

        let opaque_token = openidconnect::CsrfToken::new_random_len(32)
            .secret()
            .to_owned();
        let csrf_token = openidconnect::CsrfToken::new_random_len(32)
            .secret()
            .to_owned();
        let token_hash = digest_secret(&opaque_token).to_vec();
        let csrf_hash = digest_secret(&csrf_token).to_vec();
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| SessionError::Unavailable)?;
        let row = sqlx::query(
            "INSERT INTO litecr.access_session \
             (token_hash, csrf_hash, actor_kind, issuer_url, external_subject, oidc_session_id, expires_at) \
             VALUES ($1, $2, $3, $4, $5, $6, now() + ($7 * interval '1 second')) \
             RETURNING session_id",
        )
        .bind(token_hash)
        .bind(csrf_hash)
        .bind(actor_kind_name(actor.kind))
        .bind(actor.issuer_url.as_deref().unwrap_or_default())
        .bind(&actor.id)
        .bind(oidc_session_id)
        .bind(expires_in_seconds)
        .fetch_one(&mut *transaction)
        .await
        .map_err(|_| SessionError::Unavailable)?;
        let session_id: i64 = row
            .try_get("session_id")
            .map_err(|_| SessionError::Unavailable)?;
        sqlx::query(
            "INSERT INTO litecr.access_audit \
             (actor_kind, actor_issuer_url, actor_id, action, resource_type, resource_ref, outcome, request_id) \
             VALUES ($1, $2, $3, 'session.created', 'access_session', $4, 'allowed', $5)",
        )
        .bind(actor_kind_name(actor.kind))
        .bind(actor.issuer_url.as_deref().unwrap_or_default())
        .bind(&actor.id)
        .bind(session_id.to_string())
        .bind(request_id)
        .execute(&mut *transaction)
        .await
        .map_err(|_| SessionError::Unavailable)?;
        transaction
            .commit()
            .await
            .map_err(|_| SessionError::Unavailable)?;

        Ok(SessionCredentials {
            opaque_token,
            csrf_token,
        })
    }

    async fn resolve(
        &self,
        opaque_session_token: &str,
    ) -> Result<Option<AccessSession>, SessionError> {
        if opaque_session_token.len() < 32 || opaque_session_token.len() > 256 {
            return Ok(None);
        }
        let token_hash = digest_secret(opaque_session_token).to_vec();
        let row = sqlx::query(
            "SELECT session_id::text AS session_id, actor_kind, issuer_url, external_subject, csrf_hash \
             FROM litecr.access_session \
             WHERE token_hash = $1 AND revoked_at IS NULL AND expires_at > now() \
               AND last_seen_at > now() - ($2::bigint * interval '1 second')",
        )
        .bind(token_hash)
        .bind(self.idle_timeout_seconds)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| SessionError::Unavailable)?;
        let Some(row) = row else {
            return Ok(None);
        };
        let kind: String = row
            .try_get("actor_kind")
            .map_err(|_| SessionError::Unavailable)?;
        let kind = parse_actor_kind(&kind).map_err(|_| SessionError::Unavailable)?;
        let subject: String = row
            .try_get("external_subject")
            .map_err(|_| SessionError::Unavailable)?;
        let issuer_url: String = row
            .try_get("issuer_url")
            .map_err(|_| SessionError::Unavailable)?;
        let identity = self
            .directory
            .load_identity(
                kind,
                (!issuer_url.is_empty()).then_some(issuer_url.as_str()),
                &subject,
            )
            .await
            .map_err(|error| match error {
                AccessDirectoryError::Unavailable => SessionError::Unavailable,
                AccessDirectoryError::Inactive => SessionError::Invalid,
            })?
            .filter(|identity| identity.active && !identity.grants.is_empty());
        let Some(identity) = identity else {
            return Ok(None);
        };
        let session_id: String = row
            .try_get("session_id")
            .map_err(|_| SessionError::Unavailable)?;
        let csrf_hash: Vec<u8> = row
            .try_get("csrf_hash")
            .map_err(|_| SessionError::Unavailable)?;
        let refreshed = sqlx::query(
            "UPDATE litecr.access_session SET last_seen_at = now() \
            WHERE session_id = $1 AND revoked_at IS NULL AND expires_at > now() \
              AND last_seen_at > now() - ($2::bigint * interval '1 second')",
        )
        .bind(
            session_id
                .parse::<i64>()
                .map_err(|_| SessionError::Unavailable)?,
        )
        .bind(self.idle_timeout_seconds)
        .execute(&self.pool)
        .await
        .map_err(|_| SessionError::Unavailable)?;
        if refreshed.rows_affected() != 1 {
            return Ok(None);
        }

        Ok(Some(AccessSession {
            session_id,
            actor: identity.actor,
            grants: identity.grants,
            csrf_hash,
        }))
    }

    async fn verify_csrf(&self, session_id: &str, csrf_token: &str) -> Result<bool, SessionError> {
        if csrf_token.len() < 32 || csrf_token.len() > 256 {
            return Ok(false);
        }
        let Ok(session_id) = session_id.parse::<i64>() else {
            return Ok(false);
        };
        let expected = sqlx::query_scalar::<_, Vec<u8>>(
            "SELECT csrf_hash FROM litecr.access_session \
             WHERE session_id = $1 AND revoked_at IS NULL AND expires_at > now() \
               AND last_seen_at > now() - ($2::bigint * interval '1 second')",
        )
        .bind(session_id)
        .bind(self.idle_timeout_seconds)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| SessionError::Unavailable)?;
        let Some(expected) = expected else {
            return Ok(false);
        };
        let candidate = digest_secret(csrf_token);
        Ok(expected.as_slice().ct_eq(candidate.as_slice()).into())
    }

    async fn revoke(
        &self,
        opaque_session_token: &str,
        request_id: &str,
    ) -> Result<(), SessionError> {
        let token_hash = digest_secret(opaque_session_token).to_vec();
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| SessionError::Unavailable)?;
        let row = sqlx::query(
            "UPDATE litecr.access_session SET revoked_at = COALESCE(revoked_at, now()) \
             WHERE token_hash = $1 AND revoked_at IS NULL \
             RETURNING session_id::text AS session_id, actor_kind, issuer_url, external_subject",
        )
        .bind(token_hash)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|_| SessionError::Unavailable)?;
        if let Some(row) = row {
            let session_id: String = row
                .try_get("session_id")
                .map_err(|_| SessionError::Unavailable)?;
            let kind: String = row
                .try_get("actor_kind")
                .map_err(|_| SessionError::Unavailable)?;
            let issuer: String = row
                .try_get("issuer_url")
                .map_err(|_| SessionError::Unavailable)?;
            let actor_id: String = row
                .try_get("external_subject")
                .map_err(|_| SessionError::Unavailable)?;
            sqlx::query(
                "INSERT INTO litecr.access_audit \
                 (actor_kind, actor_issuer_url, actor_id, action, resource_type, resource_ref, outcome, request_id) \
                 VALUES ($1, $2, $3, 'session.revoked', 'access_session', $4, 'revoked', $5)",
            )
            .bind(kind)
            .bind(issuer)
            .bind(actor_id)
            .bind(session_id)
            .bind(request_id)
            .execute(&mut *transaction)
            .await
            .map_err(|_| SessionError::Unavailable)?;
        }
        transaction
            .commit()
            .await
            .map_err(|_| SessionError::Unavailable)?;
        Ok(())
    }

    async fn revoke_actor_sessions(
        &self,
        actor: &ActorRef,
        request_id: &str,
    ) -> Result<u64, SessionError> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| SessionError::Unavailable)?;
        let result = sqlx::query(
            "UPDATE litecr.access_session SET revoked_at = COALESCE(revoked_at, now()) \
             WHERE actor_kind = $1 AND issuer_url = $2 AND external_subject = $3 AND revoked_at IS NULL \
             RETURNING session_id",
        )
        .bind(actor_kind_name(actor.kind))
        .bind(actor.issuer_url.as_deref().unwrap_or_default())
        .bind(&actor.id)
        .fetch_all(&mut *transaction)
        .await
        .map_err(|_| SessionError::Unavailable)?;
        if !result.is_empty() {
            sqlx::query(
                "INSERT INTO litecr.access_audit \
                 (actor_kind, actor_issuer_url, actor_id, action, resource_type, resource_ref, outcome, reason_code, request_id) \
                 VALUES ($1, $2, $3, 'session.revoke_actor', 'access_session', $4, 'revoked', 'actor_sessions_revoked', $5)",
            )
            .bind(actor_kind_name(actor.kind))
            .bind(actor.issuer_url.as_deref().unwrap_or_default())
            .bind(&actor.id)
            .bind(result.len().to_string())
            .bind(request_id)
            .execute(&mut *transaction)
            .await
            .map_err(|_| SessionError::Unavailable)?;
        }
        transaction
            .commit()
            .await
            .map_err(|_| SessionError::Unavailable)?;
        Ok(result.len() as u64)
    }
}

#[derive(Clone)]
pub struct PostgresAccessAuthenticator {
    sessions: PostgresAccessSessionRepository,
}

impl PostgresAccessAuthenticator {
    pub fn new(sessions: PostgresAccessSessionRepository) -> Self {
        Self { sessions }
    }
}

#[async_trait]
impl AccessAuthenticator for PostgresAccessAuthenticator {
    async fn authenticate_cookie(
        &self,
        opaque_session_token: &str,
        csrf_token: Option<&str>,
        require_csrf: bool,
        request_id: &str,
    ) -> Result<AccessIdentity, SessionError> {
        let Some(session) = self.sessions.resolve(opaque_session_token).await? else {
            return Err(SessionError::Invalid);
        };
        if require_csrf {
            let token = csrf_token.ok_or(SessionError::Invalid)?;
            if !self
                .sessions
                .verify_csrf(&session.session_id, token)
                .await?
            {
                return Err(SessionError::Invalid);
            }
        }
        let _ = request_id;
        Ok(AccessIdentity {
            actor: session.actor,
            active: true,
            grants: session.grants,
        })
    }

    async fn authenticate_integration_credential(
        &self,
        opaque_credential: &str,
        request_id: &str,
    ) -> Result<AccessIdentity, SessionError> {
        if opaque_credential.len() < 32 || opaque_credential.len() > 256 {
            return Err(SessionError::Invalid);
        }
        let row = sqlx::query(
            "UPDATE litecr.access_integration_credential \
             SET last_used_at = now() \
             WHERE token_hash = $1 AND revoked_at IS NULL \
                 AND (expires_at IS NULL OR expires_at > now()) \
             RETURNING issuer_url, external_subject",
        )
        .bind(digest_secret(opaque_credential).to_vec())
        .fetch_optional(&self.sessions.pool)
        .await
        .map_err(|_| SessionError::Unavailable)?
        .ok_or(SessionError::Invalid)?;
        let issuer_url: String = row
            .try_get("issuer_url")
            .map_err(|_| SessionError::Unavailable)?;
        let external_subject: String = row
            .try_get("external_subject")
            .map_err(|_| SessionError::Unavailable)?;
        let identity = self
            .sessions
            .directory
            .load_identity(
                ActorKind::IntegrationClient,
                (!issuer_url.is_empty()).then_some(issuer_url.as_str()),
                &external_subject,
            )
            .await
            .map_err(|error| match error {
                AccessDirectoryError::Unavailable => SessionError::Unavailable,
                AccessDirectoryError::Inactive => SessionError::Invalid,
            })?
            .filter(|identity| {
                identity.active
                    && !identity.grants.is_empty()
                    && identity.grants.iter().all(|grant| {
                        grant
                            .roles
                            .iter()
                            .all(|role| *role == Role::IntegrationClient)
                    })
            })
            .ok_or(SessionError::Invalid)?;
        sqlx::query(
            "INSERT INTO litecr.access_audit \
             (actor_kind, actor_issuer_url, actor_id, action, outcome, request_id) \
             VALUES ('integration_client', $1, $2, 'integration.authenticated', 'allowed', $3)",
        )
        .bind(&issuer_url)
        .bind(&external_subject)
        .bind(request_id)
        .execute(&self.sessions.pool)
        .await
        .map_err(|_| SessionError::Unavailable)?;
        let _ = request_id;
        Ok(identity)
    }
}

#[derive(Clone)]
pub struct PostgresOidcFlowStore {
    pool: PgPool,
}

impl PostgresOidcFlowStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl OidcFlowStore for PostgresOidcFlowStore {
    async fn create(
        &self,
        flow: &OidcFlowSecrets,
        expires_in_seconds: i64,
    ) -> Result<(), SessionError> {
        if expires_in_seconds <= 0 || expires_in_seconds > 900 {
            return Err(SessionError::Invalid);
        }
        sqlx::query(
            "INSERT INTO litecr.oidc_login_transaction \
             (transaction_hash, state_hash, nonce, pkce_verifier, redirect_uri, expires_at) \
             VALUES ($1, $2, $3, $4, $5, now() + ($6 * interval '1 second'))",
        )
        .bind(digest_secret(&flow.flow_id).to_vec())
        .bind(digest_secret(&flow.state).to_vec())
        .bind(&flow.nonce)
        .bind(&flow.pkce_verifier)
        .bind(&flow.redirect_uri)
        .bind(expires_in_seconds)
        .execute(&self.pool)
        .await
        .map_err(|_| SessionError::Unavailable)?;
        Ok(())
    }

    async fn consume(&self, flow_id: &str, state: &str) -> Result<Option<OidcFlow>, SessionError> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| SessionError::Unavailable)?;
        let row = sqlx::query(
            "SELECT transaction_hash, state_hash, nonce, pkce_verifier, redirect_uri \
             FROM litecr.oidc_login_transaction \
             WHERE transaction_hash = $1 AND expires_at > now() AND consumed_at IS NULL \
             FOR UPDATE",
        )
        .bind(digest_secret(flow_id).to_vec())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|_| SessionError::Unavailable)?;
        let Some(row) = row else {
            transaction
                .rollback()
                .await
                .map_err(|_| SessionError::Unavailable)?;
            return Ok(None);
        };
        let expected_state: Vec<u8> = row
            .try_get("state_hash")
            .map_err(|_| SessionError::Unavailable)?;
        let nonce: String = row
            .try_get::<Option<String>, _>("nonce")
            .map_err(|_| SessionError::Unavailable)?
            .ok_or(SessionError::Invalid)?;
        if expected_state
            .as_slice()
            .ct_eq(digest_secret(state).as_slice())
            .unwrap_u8()
            != 1
        {
            transaction
                .rollback()
                .await
                .map_err(|_| SessionError::Unavailable)?;
            return Ok(None);
        }
        let pkce_verifier: String = row
            .try_get::<Option<String>, _>("pkce_verifier")
            .map_err(|_| SessionError::Unavailable)?
            .ok_or(SessionError::Invalid)?;
        let redirect_uri: String = row
            .try_get("redirect_uri")
            .map_err(|_| SessionError::Unavailable)?;
        sqlx::query("UPDATE litecr.oidc_login_transaction SET consumed_at = now(), nonce = NULL, pkce_verifier = NULL WHERE transaction_hash = $1")
            .bind(digest_secret(flow_id).to_vec())
            .execute(&mut *transaction)
            .await
            .map_err(|_| SessionError::Unavailable)?;
        transaction
            .commit()
            .await
            .map_err(|_| SessionError::Unavailable)?;
        Ok(Some(OidcFlow {
            flow_id: flow_id.to_owned(),
            authorization: OidcAuthorization {
                flow_id: flow_id.to_owned(),
                authorization_url: String::new(),
                redirect_uri,
                state: state.to_owned(),
                nonce,
                pkce_verifier,
            },
        }))
    }
}

#[derive(Clone)]
pub struct OidcProviderConfig {
    pub issuer_url: String,
    pub client_id: String,
    pub client_secret: Option<String>,
    pub redirect_uri: String,
    /// Must be enabled only by an explicit local development/test profile.
    pub allow_http_loopback: bool,
}

#[derive(Clone)]
pub struct OidcProviderClient {
    metadata: CoreProviderMetadata,
    http: openidconnect::reqwest::Client,
    config: OidcProviderConfig,
}

impl OidcProviderClient {
    pub async fn discover(config: OidcProviderConfig) -> Result<Self, OidcProviderError> {
        let issuer = IssuerUrl::new(config.issuer_url.clone())
            .map_err(|_| OidcProviderError::InvalidResponse)?;
        let issuer_url = issuer.url().clone();
        if !issuer_transport_is_allowed(
            issuer_url.scheme(),
            issuer_url.host_str(),
            config.allow_http_loopback,
        ) {
            return Err(OidcProviderError::InvalidResponse);
        }
        let http = openidconnect::reqwest::ClientBuilder::new()
            .redirect(openidconnect::reqwest::redirect::Policy::none())
            .connect_timeout(std::time::Duration::from_secs(5))
            .timeout(std::time::Duration::from_secs(10))
            // Respect CA roots installed by the system administrator (for
            // example, a hospital's private OIDC CA) while retaining normal
            // TLS certificate and hostname validation.
            .tls_built_in_native_certs(true)
            .build()
            .map_err(|_| OidcProviderError::Unavailable)?;
        let metadata = CoreProviderMetadata::discover_async(issuer, &http)
            .await
            .map_err(|_| OidcProviderError::Unavailable)?;
        if metadata.issuer().url().as_str() != config.issuer_url {
            return Err(OidcProviderError::InvalidResponse);
        }
        let redirect_uri = RedirectUrl::new(config.redirect_uri.clone())
            .map_err(|_| OidcProviderError::InvalidResponse)?;
        let _ = CoreClient::from_provider_metadata(
            metadata.clone(),
            ClientId::new(config.client_id.clone()),
            config.client_secret.clone().map(ClientSecret::new),
        )
        .set_redirect_uri(redirect_uri);
        Ok(Self {
            metadata,
            http,
            config,
        })
    }
}

#[async_trait]
impl OidcProvider for OidcProviderClient {
    async fn begin_authorization(&self) -> Result<OidcAuthorization, OidcProviderError> {
        let flow_id = CsrfToken::new_random().secret().to_owned();
        let redirect_uri = RedirectUrl::new(self.config.redirect_uri.clone())
            .map_err(|_| OidcProviderError::InvalidResponse)?;
        let client = CoreClient::from_provider_metadata(
            self.metadata.clone(),
            ClientId::new(self.config.client_id.clone()),
            self.config.client_secret.clone().map(ClientSecret::new),
        )
        .set_redirect_uri(redirect_uri);
        let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
        let (url, state, nonce) = client
            .authorize_url(
                CoreAuthenticationFlow::AuthorizationCode,
                CsrfToken::new_random,
                Nonce::new_random,
            )
            .add_scope(Scope::new("openid".to_owned()))
            .set_pkce_challenge(challenge)
            .url();
        Ok(OidcAuthorization {
            flow_id,
            authorization_url: url.to_string(),
            redirect_uri: self.config.redirect_uri.clone(),
            state: state.secret().to_owned(),
            nonce: nonce.secret().to_owned(),
            pkce_verifier: verifier.secret().to_owned(),
        })
    }

    async fn exchange_code(
        &self,
        code: &str,
        flow: &OidcFlow,
    ) -> Result<litecr_application::access::OidcIdentityClaims, OidcProviderError> {
        let redirect_uri = RedirectUrl::new(self.config.redirect_uri.clone())
            .map_err(|_| OidcProviderError::InvalidResponse)?;
        let client = CoreClient::from_provider_metadata(
            self.metadata.clone(),
            ClientId::new(self.config.client_id.clone()),
            self.config.client_secret.clone().map(ClientSecret::new),
        )
        .set_redirect_uri(redirect_uri);
        let response = client
            .exchange_code(AuthorizationCode::new(code.to_owned()))
            .map_err(|_| OidcProviderError::InvalidResponse)?
            .set_pkce_verifier(PkceCodeVerifier::new(
                flow.authorization.pkce_verifier.clone(),
            ))
            .request_async(&self.http)
            .await
            .map_err(|_| OidcProviderError::Rejected)?;
        let id_token = response.id_token().ok_or(OidcProviderError::Rejected)?;
        let claims = id_token
            .claims(
                &client.id_token_verifier(),
                &Nonce::new(flow.authorization.nonce.clone()),
            )
            .map_err(|_| OidcProviderError::Rejected)?;
        let issuer = claims.issuer().url().as_str().to_owned();
        let audiences = claims
            .audiences()
            .iter()
            .map(|audience| audience.as_str().to_owned())
            .collect::<Vec<_>>();
        let authorized_party = claims.authorized_party().map(|value| value.as_str());
        let token_compact = id_token.to_string();
        let now_epoch_seconds = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| OidcProviderError::Rejected)?
            .as_secs() as i64;
        if !verified_issuer_and_audience_are_valid(
            &issuer,
            &self.config.issuer_url,
            &audiences,
            authorized_party,
            &self.config.client_id,
        ) || !verified_token_nbf_is_current_or_absent(&token_compact, now_epoch_seconds)
        {
            return Err(OidcProviderError::Rejected);
        }
        let expiration = claims.expiration().to_owned();
        let expires_at_epoch_seconds = std::time::SystemTime::from(expiration)
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| OidcProviderError::Rejected)?
            .as_secs() as i64;
        Ok(litecr_application::access::OidcIdentityClaims {
            issuer,
            subject: claims.subject().as_str().to_owned(),
            audience: self.config.client_id.clone(),
            expires_at_epoch_seconds,
            nonce: claims
                .nonce()
                .map(|value| value.secret().to_owned())
                .ok_or(OidcProviderError::Rejected)?,
            session_id: None,
            email_verified: claims.email_verified(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{
        audience_and_authorized_party_are_valid, digest_secret, issuer_transport_is_allowed,
        verified_issuer_and_audience_are_valid, verified_token_nbf_is_current_or_absent,
    };
    use openidconnect::CsrfToken;

    #[tokio::test]
    async fn session_idle_policy_has_a_bounded_default() {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://synthetic@127.0.0.1/no_connection")
            .unwrap();
        let repository = super::PostgresAccessSessionRepository::new(
            pool.clone(),
            super::PostgresAccessDirectory::new(pool),
        );
        assert_eq!(repository.idle_timeout_seconds, 900);
        for seconds in [0, 59, 1801, i64::MAX] {
            assert!(
                repository
                    .clone()
                    .with_idle_timeout_seconds(seconds)
                    .is_none()
            );
        }
        for seconds in [60, 900, 1800] {
            assert!(
                repository
                    .clone()
                    .with_idle_timeout_seconds(seconds)
                    .is_some()
            );
        }
    }

    fn untrusted_token(payload: &str) -> String {
        use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
        format!("header.{}.signature", URL_SAFE_NO_PAD.encode(payload))
    }

    #[test]
    fn secret_digest_is_fixed_width_and_one_way_shape() {
        let digest = digest_secret("synthetic-session-token");
        assert_eq!(digest.len(), 32);
        assert_ne!(digest.as_slice(), b"synthetic-session-token");
    }

    #[test]
    fn nbf_is_checked_after_verification_as_a_rejection_only_claim() {
        assert!(verified_token_nbf_is_current_or_absent(
            &untrusted_token(r#"{"sub":"synthetic"}"#),
            1_000,
        ));
        assert!(verified_token_nbf_is_current_or_absent(
            &untrusted_token(r#"{"nbf":999}"#),
            1_000,
        ));
        assert!(!verified_token_nbf_is_current_or_absent(
            &untrusted_token(r#"{"nbf":1001}"#),
            1_000,
        ));
        assert!(!verified_token_nbf_is_current_or_absent(
            &untrusted_token(r#"{"nbf":"tomorrow"}"#),
            1_000,
        ));
        assert!(!verified_token_nbf_is_current_or_absent("malformed", 1_000));
    }

    #[test]
    fn audience_must_be_the_single_configured_client_and_azp_must_match() {
        assert!(audience_and_authorized_party_are_valid(
            &["litecr-web".to_owned()],
            None,
            "litecr-web",
        ));
        assert!(audience_and_authorized_party_are_valid(
            &["litecr-web".to_owned()],
            Some("litecr-web"),
            "litecr-web",
        ));
        assert!(!audience_and_authorized_party_are_valid(
            &["litecr-web".to_owned(), "another-client".to_owned()],
            Some("litecr-web"),
            "litecr-web",
        ));
        assert!(!audience_and_authorized_party_are_valid(
            &["litecr-web".to_owned()],
            Some("another-client"),
            "litecr-web",
        ));
        assert!(!audience_and_authorized_party_are_valid(
            &["another-client".to_owned()],
            None,
            "litecr-web",
        ));
    }

    #[test]
    fn verified_issuer_must_exactly_match_discovered_configuration() {
        let audiences = vec!["litecr-web".to_owned()];
        assert!(verified_issuer_and_audience_are_valid(
            "https://idp.example.test",
            "https://idp.example.test",
            &audiences,
            None,
            "litecr-web",
        ));
        assert!(!verified_issuer_and_audience_are_valid(
            "https://attacker.example.test",
            "https://idp.example.test",
            &audiences,
            None,
            "litecr-web",
        ));
        assert!(!verified_issuer_and_audience_are_valid(
            "https://idp.example.test",
            "https://idp.example.test",
            &["another-client".to_owned()],
            None,
            "litecr-web",
        ));
    }

    #[test]
    fn plaintext_oidc_issuer_requires_explicit_loopback_development_flag() {
        assert!(!issuer_transport_is_allowed(
            "http",
            Some("127.0.0.1"),
            false
        ));
        assert!(issuer_transport_is_allowed("http", Some("127.0.0.1"), true));
        assert!(!issuer_transport_is_allowed(
            "http",
            Some("idp.example.test"),
            true
        ));
        assert!(issuer_transport_is_allowed(
            "https",
            Some("idp.example.test"),
            false
        ));
    }

    #[test]
    fn generated_oidc_flow_state_has_128_bits_of_entropy() {
        assert_eq!(CsrfToken::new_random().secret().len(), 22);
    }
}
