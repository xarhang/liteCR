use async_trait::async_trait;
use thiserror::Error;

use super::{ActorKind, ActorRef, Permission, Role, ScopeGrant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecurityEventType {
    OidcCallbackDenied,
    SessionAuthenticationDenied,
    ProtectedRouteDenied,
}

impl SecurityEventType {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OidcCallbackDenied => "oidc_callback_denied",
            Self::SessionAuthenticationDenied => "session_authentication_denied",
            Self::ProtectedRouteDenied => "protected_route_denied",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecurityEventReason {
    InvalidCallback,
    InvalidLoginFlow,
    IdentityProviderRejected,
    InvalidIdentity,
    IdentityNotProvisioned,
    MissingCredential,
    InvalidCredential,
    ConflictingCredentials,
    CsrfRejected,
    PermissionDenied,
}

impl SecurityEventReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidCallback => "invalid_callback",
            Self::InvalidLoginFlow => "invalid_login_flow",
            Self::IdentityProviderRejected => "identity_provider_rejected",
            Self::InvalidIdentity => "invalid_identity",
            Self::IdentityNotProvisioned => "identity_not_provisioned",
            Self::MissingCredential => "missing_credential",
            Self::InvalidCredential => "invalid_credential",
            Self::ConflictingCredentials => "conflicting_credentials",
            Self::CsrfRejected => "csrf_rejected",
            Self::PermissionDenied => "permission_denied",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecurityRouteFamily {
    OidcCallback,
    Session,
    Logout,
    Registry,
    Matching,
    Review,
    Migration,
    Configuration,
    FhirPatient,
    FhirMetadata,
    OtherProtected,
}

impl SecurityRouteFamily {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OidcCallback => "oidc_callback",
            Self::Session => "session",
            Self::Logout => "logout",
            Self::Registry => "registry",
            Self::Matching => "matching",
            Self::Review => "review",
            Self::Migration => "migration",
            Self::Configuration => "configuration",
            Self::FhirPatient => "fhir_patient",
            Self::FhirMetadata => "fhir_metadata",
            Self::OtherProtected => "other_protected",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecurityScopeShape {
    None,
    RequestBound,
    PathRecord,
    Unknown,
}

impl SecurityScopeShape {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::RequestBound => "request_bound",
            Self::PathRecord => "path_record",
            Self::Unknown => "unknown",
        }
    }
}

/// Deliberately contains no request URL, user-controlled path, query, headers,
/// body, OIDC values, credentials, or patient/resource identifiers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecurityAuditEvent {
    pub event_type: SecurityEventType,
    pub reason: SecurityEventReason,
    pub route_family: SecurityRouteFamily,
    pub scope_shape: SecurityScopeShape,
    /// Set only when an authenticated, provisioned actor is already known.
    pub actor: Option<ActorRef>,
    pub request_id: String,
}

impl SecurityAuditEvent {
    pub fn validate(&self) -> bool {
        !self.request_id.is_empty()
            && self.request_id.len() <= 128
            && self.request_id.bytes().all(|byte| byte.is_ascii_graphic())
            && self.actor.as_ref().is_none_or(|actor| {
                !actor.id.is_empty()
                    && actor.id.len() <= 512
                    && actor.id.bytes().all(|byte| !byte.is_ascii_control())
            })
    }
}

#[derive(Debug, thiserror::Error)]
#[error("security audit unavailable")]
pub struct SecurityAuditError;

#[async_trait]
pub trait AccessSecurityAudit: Send + Sync + 'static {
    async fn append(&self, event: &SecurityAuditEvent) -> Result<(), SecurityAuditError>;
}

/// Explicit adapter for unit tests and non-serving fixtures only. Production
/// composition wires the PostgreSQL append-only repository.
pub struct NoopAccessSecurityAudit;

#[async_trait]
impl AccessSecurityAudit for NoopAccessSecurityAudit {
    async fn append(&self, _event: &SecurityAuditEvent) -> Result<(), SecurityAuditError> {
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessGrant {
    pub registry_id: String,
    pub organization_id: String,
    pub record_ids: Option<Vec<String>>,
    pub roles: Vec<Role>,
    pub permissions: Vec<Permission>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessIdentity {
    pub actor: ActorRef,
    pub active: bool,
    pub grants: Vec<AccessGrant>,
}

impl AccessIdentity {
    pub fn into_scope_grants(self) -> Vec<ScopeGrant> {
        use std::collections::BTreeSet;
        self.grants
            .into_iter()
            .map(|grant| ScopeGrant {
                registry_id: grant.registry_id,
                organization_id: grant.organization_id,
                record_ids: grant.record_ids.map(|items| items.into_iter().collect()),
                roles: grant.roles.into_iter().collect::<BTreeSet<_>>(),
                permissions: grant.permissions.into_iter().collect::<BTreeSet<_>>(),
            })
            .collect()
    }
}

#[derive(Debug, Error)]
pub enum AccessDirectoryError {
    #[error("access directory unavailable")]
    Unavailable,
    #[error("identity is inactive")]
    Inactive,
}

#[async_trait]
pub trait AccessDirectory: Send + Sync + 'static {
    async fn load_identity(
        &self,
        kind: ActorKind,
        issuer_url: Option<&str>,
        external_subject: &str,
    ) -> Result<Option<AccessIdentity>, AccessDirectoryError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessSession {
    pub session_id: String,
    pub actor: ActorRef,
    pub grants: Vec<AccessGrant>,
    pub csrf_hash: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionCredentials {
    pub opaque_token: String,
    pub csrf_token: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginChallenge {
    pub authorization_url: String,
    pub flow_cookie: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginResult {
    pub session: SessionCredentials,
    pub actor: ActorRef,
}

#[derive(Debug, Error)]
pub enum SessionError {
    #[error("session unavailable")]
    Unavailable,
    #[error("session invalid or expired")]
    Invalid,
}

#[async_trait]
pub trait AccessSessionRepository: Send + Sync + 'static {
    async fn create(
        &self,
        actor: &ActorRef,
        oidc_session_id: Option<&str>,
        expires_in_seconds: i64,
        request_id: &str,
    ) -> Result<SessionCredentials, SessionError>;
    async fn resolve(
        &self,
        opaque_session_token: &str,
    ) -> Result<Option<AccessSession>, SessionError>;
    async fn verify_csrf(&self, session_id: &str, csrf_token: &str) -> Result<bool, SessionError>;
    async fn revoke(
        &self,
        opaque_session_token: &str,
        request_id: &str,
    ) -> Result<(), SessionError>;
    async fn revoke_actor_sessions(
        &self,
        actor: &ActorRef,
        request_id: &str,
    ) -> Result<u64, SessionError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OidcAuthorization {
    pub flow_id: String,
    pub authorization_url: String,
    pub redirect_uri: String,
    pub state: String,
    pub nonce: String,
    pub pkce_verifier: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OidcFlow {
    pub flow_id: String,
    pub authorization: OidcAuthorization,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OidcFlowSecrets {
    pub flow_id: String,
    pub state: String,
    pub nonce: String,
    pub pkce_verifier: String,
    pub redirect_uri: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OidcIdentityClaims {
    pub issuer: String,
    pub subject: String,
    pub audience: String,
    pub expires_at_epoch_seconds: i64,
    pub nonce: String,
    pub session_id: Option<String>,
    pub email_verified: Option<bool>,
}

#[derive(Debug, Error)]
pub enum OidcProviderError {
    #[error("identity provider unavailable")]
    Unavailable,
    #[error("identity response invalid")]
    InvalidResponse,
    #[error("identity token rejected")]
    Rejected,
}

#[async_trait]
pub trait OidcProvider: Send + Sync + 'static {
    async fn begin_authorization(&self) -> Result<OidcAuthorization, OidcProviderError>;
    async fn exchange_code(
        &self,
        code: &str,
        flow: &OidcFlow,
    ) -> Result<OidcIdentityClaims, OidcProviderError>;
}

#[async_trait]
pub trait OidcFlowStore: Send + Sync + 'static {
    async fn create(
        &self,
        flow: &OidcFlowSecrets,
        expires_in_seconds: i64,
    ) -> Result<(), SessionError>;
    /// Atomically validates the state and consumes the flow exactly once.
    async fn consume(&self, flow_id: &str, state: &str) -> Result<Option<OidcFlow>, SessionError>;
}

#[async_trait]
pub trait AccessAuthenticator: Send + Sync + 'static {
    async fn authenticate_cookie(
        &self,
        opaque_session_token: &str,
        csrf_token: Option<&str>,
        require_csrf: bool,
        request_id: &str,
    ) -> Result<AccessIdentity, SessionError>;
    async fn authenticate_integration_credential(
        &self,
        opaque_credential: &str,
        request_id: &str,
    ) -> Result<AccessIdentity, SessionError>;
}
