//! Disabled-by-default MCP credential and invocation-audit boundary.
//! No transport or registry tool is wired to these contracts yet.

use std::{collections::BTreeSet, sync::Arc};

use async_trait::async_trait;
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::{
    AccessContext, AccessDirectory, AccessIdentity, AccessServiceError, ActorKind, ActorRef,
    Permission, ResourceScope, Role, SessionError,
};

pub const MCP_STDIO_AUDIENCE: &str = "urn:litecr:mcp:stdio";
pub const MCP_CREDENTIAL_MAX_TTL_SECONDS: i64 = 900;
pub const MCP_MAX_AUDIT_RESULT_COUNT: u16 = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum McpToolId {
    RegistryGetRecordSummary,
    RegistrySearchRecords,
    MatchingRequestCandidates,
    MatchingGetEvidenceSummary,
    ReviewGetCaseSummary,
}

impl McpToolId {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::RegistryGetRecordSummary => "registry.get_record_summary",
            Self::RegistrySearchRecords => "registry.search_records",
            Self::MatchingRequestCandidates => "matching.request_candidates",
            Self::MatchingGetEvidenceSummary => "matching.get_evidence_summary",
            Self::ReviewGetCaseSummary => "review.get_case_summary",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "registry.get_record_summary" => Self::RegistryGetRecordSummary,
            "registry.search_records" => Self::RegistrySearchRecords,
            "matching.request_candidates" => Self::MatchingRequestCandidates,
            "matching.get_evidence_summary" => Self::MatchingGetEvidenceSummary,
            "review.get_case_summary" => Self::ReviewGetCaseSummary,
            _ => return None,
        })
    }

    pub const fn required_permission(self) -> Permission {
        match self {
            Self::RegistryGetRecordSummary => Permission::PersonRead,
            Self::RegistrySearchRecords => Permission::PersonSearch,
            Self::MatchingRequestCandidates => Permission::MatchRequest,
            Self::MatchingGetEvidenceSummary => Permission::MatchEvidenceRead,
            Self::ReviewGetCaseSummary => Permission::ReviewRead,
        }
    }
}

/// Provision-time values only. No request field can create or widen this profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpCredentialProfile {
    pub client_id: String,
    pub purpose_code: String,
    pub allowed_tools: BTreeSet<McpToolId>,
    pub ttl_seconds: i64,
}

impl McpCredentialProfile {
    pub fn new(
        client_id: String,
        purpose_code: String,
        allowed_tools: impl IntoIterator<Item = McpToolId>,
        ttl_seconds: i64,
    ) -> Option<Self> {
        let allowed_tools = allowed_tools.into_iter().collect::<BTreeSet<_>>();
        (valid_binding_code(&client_id)
            && valid_binding_code(&purpose_code)
            && !allowed_tools.is_empty()
            && ttl_seconds > 0
            && ttl_seconds <= MCP_CREDENTIAL_MAX_TTL_SECONDS)
            .then_some(Self {
                client_id,
                purpose_code,
                allowed_tools,
                ttl_seconds,
            })
    }
}

fn valid_binding_code(value: &str) -> bool {
    (1..=96).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._:-".contains(&byte))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpCredentialBinding {
    pub client_id: String,
    pub purpose_code: String,
    pub allowed_tools: BTreeSet<McpToolId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpCredentialIdentity {
    pub identity: AccessIdentity,
    pub binding: McpCredentialBinding,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpAuthenticatedClient {
    context: AccessContext,
    binding: McpCredentialBinding,
}

impl McpAuthenticatedClient {
    fn context_for_tool(&self, tool: McpToolId) -> Option<AccessContext> {
        if !self.allows_tool(tool) {
            return None;
        }
        self.context.only_permission(tool.required_permission())
    }

    pub fn binding(&self) -> &McpCredentialBinding {
        &self.binding
    }

    pub fn allows_tool(&self, tool: McpToolId) -> bool {
        self.binding.allowed_tools.contains(&tool)
    }

    /// Creates the only success-audit input accepted by the reply boundary.
    /// Attribution is copied from this authenticated client and its exact
    /// tool-specific grant; caller-provided tenant values must match a current
    /// organization-wide grant because the audit intentionally stores no
    /// record identifier.
    pub fn invocation_for_tool(
        &self,
        tool: McpToolId,
        registry_id: &str,
        organization_id: &str,
    ) -> Option<McpAuthorizedInvocation> {
        if !valid_scope_reference(registry_id) || !valid_scope_reference(organization_id) {
            return None;
        }
        let context = self.context_for_tool(tool)?;
        let scope = ResourceScope {
            registry_id: registry_id.to_owned(),
            organization_id: organization_id.to_owned(),
            record_id: None,
        };
        if !context
            .grants()
            .iter()
            .any(|grant| grant.record_ids.is_none() && grant.permits_scope(&scope))
        {
            return None;
        }
        let context = context.only_organization_scope(registry_id, organization_id)?;
        Some(McpAuthorizedInvocation {
            request_id: audit_request_reference(context.request_id()),
            client_id: self.binding.client_id.clone(),
            purpose_code: self.binding.purpose_code.clone(),
            actor: context.actor().clone(),
            tool_id: tool,
            registry_id: registry_id.to_owned(),
            organization_id: organization_id.to_owned(),
            context,
        })
    }
}

fn audit_request_reference(request_id: &str) -> String {
    let digest = Sha256::digest(request_id.as_bytes());
    let mut encoded = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(encoded, "{byte:02x}");
    }
    encoded
}

#[async_trait]
pub trait McpCredentialStore: Send + Sync + 'static {
    async fn issue(
        &self,
        actor: &ActorRef,
        profile: &McpCredentialProfile,
        request_id: &str,
    ) -> Result<String, SessionError>;

    /// Resolves the opaque secret and reloads the actor's current directory grants.
    async fn authenticate(
        &self,
        opaque_credential: &str,
        request_id: &str,
    ) -> Result<McpCredentialIdentity, SessionError>;

    async fn revoke(&self, opaque_credential: &str, request_id: &str)
    -> Result<bool, SessionError>;
}

pub struct McpAccessService {
    directory: Arc<dyn AccessDirectory>,
    credentials: Arc<dyn McpCredentialStore>,
}

impl McpAccessService {
    pub fn new(
        directory: Arc<dyn AccessDirectory>,
        credentials: Arc<dyn McpCredentialStore>,
    ) -> Self {
        Self {
            directory,
            credentials,
        }
    }

    /// Provisioning only. The caller must supply client/purpose/tool values that
    /// were reviewed out of band; this method is deliberately not wired to a route.
    pub async fn issue(
        &self,
        issuer_url: Option<&str>,
        external_subject: &str,
        profile: &McpCredentialProfile,
        request_id: &str,
    ) -> Result<String, AccessServiceError> {
        if !profile_is_valid(profile) || !valid_request_id(request_id) {
            return Err(AccessServiceError::Rejected);
        }
        let identity = self
            .directory
            .load_identity(ActorKind::IntegrationClient, issuer_url, external_subject)
            .await
            .map_err(|_| AccessServiceError::Unavailable)?
            .filter(valid_integration_identity)
            .ok_or(AccessServiceError::Rejected)?;
        if !tools_are_granted(&profile.allowed_tools, &identity.grants) {
            return Err(AccessServiceError::Rejected);
        }
        self.credentials
            .issue(&identity.actor, profile, request_id)
            .await
            .map_err(map_session_error)
    }

    pub async fn authenticate(
        &self,
        opaque_credential: &str,
        request_id: &str,
    ) -> Result<McpAuthenticatedClient, SessionError> {
        if !valid_request_id(request_id) {
            return Err(SessionError::Invalid);
        }
        let credential = self
            .credentials
            .authenticate(opaque_credential, request_id)
            .await?;
        if !valid_integration_identity(&credential.identity)
            || !valid_binding_code(&credential.binding.client_id)
            || !valid_binding_code(&credential.binding.purpose_code)
            || credential.binding.allowed_tools.is_empty()
        {
            return Err(SessionError::Invalid);
        }
        let current_tools = credential
            .binding
            .allowed_tools
            .into_iter()
            .filter(|tool| tool_is_granted(*tool, &credential.identity.grants))
            .collect::<BTreeSet<_>>();
        if current_tools.is_empty() {
            return Err(SessionError::Invalid);
        }
        let mut identity = credential.identity;
        let allowed_permissions = current_tools
            .iter()
            .map(|tool| tool.required_permission())
            .collect::<BTreeSet<_>>();
        for grant in &mut identity.grants {
            grant
                .permissions
                .retain(|permission| allowed_permissions.contains(permission));
        }
        identity
            .grants
            .retain(|grant| !grant.permissions.is_empty());
        if identity.grants.is_empty() {
            return Err(SessionError::Invalid);
        }
        let binding = McpCredentialBinding {
            client_id: credential.binding.client_id,
            purpose_code: credential.binding.purpose_code.clone(),
            allowed_tools: current_tools,
        };
        let context = AccessContext::from_authorized_identity(
            identity.actor.clone(),
            identity.into_scope_grants(),
            request_id.to_owned(),
            None,
            Some(binding.purpose_code.clone()),
        )
        .ok_or(SessionError::Invalid)?;
        Ok(McpAuthenticatedClient { context, binding })
    }

    pub async fn revoke(
        &self,
        opaque_credential: &str,
        request_id: &str,
    ) -> Result<bool, SessionError> {
        if !valid_request_id(request_id) {
            return Err(SessionError::Invalid);
        }
        self.credentials.revoke(opaque_credential, request_id).await
    }
}

fn map_session_error(error: SessionError) -> AccessServiceError {
    match error {
        SessionError::Unavailable => AccessServiceError::Unavailable,
        SessionError::Invalid => AccessServiceError::Rejected,
    }
}

fn profile_is_valid(profile: &McpCredentialProfile) -> bool {
    valid_binding_code(&profile.client_id)
        && valid_binding_code(&profile.purpose_code)
        && !profile.allowed_tools.is_empty()
        && profile.ttl_seconds > 0
        && profile.ttl_seconds <= MCP_CREDENTIAL_MAX_TTL_SECONDS
}

fn valid_request_id(value: &str) -> bool {
    (1..=128).contains(&value.len()) && value.bytes().all(|byte| byte.is_ascii_graphic())
}

fn valid_integration_identity(identity: &AccessIdentity) -> bool {
    identity.actor.kind == ActorKind::IntegrationClient
        && identity.active
        && !identity.grants.is_empty()
        && identity.grants.iter().all(|grant| {
            grant
                .roles
                .iter()
                .all(|role| *role == Role::IntegrationClient)
        })
}

fn tool_is_granted(tool: McpToolId, grants: &[super::AccessGrant]) -> bool {
    grants.iter().any(|grant| {
        grant
            .roles
            .iter()
            .all(|role| *role == Role::IntegrationClient)
            && grant.permissions.contains(&tool.required_permission())
    })
}

fn tools_are_granted(tools: &BTreeSet<McpToolId>, grants: &[super::AccessGrant]) -> bool {
    tools.iter().all(|tool| tool_is_granted(*tool, grants))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpInvocationOutcome {
    Succeeded,
    Denied,
}

impl McpInvocationOutcome {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Denied => "denied",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpInvocationReason {
    None,
    Unauthenticated,
    InvalidCredential,
    ToolNotAllowed,
    AccessDenied,
    NotFound,
    InvalidInput,
    Unavailable,
}

impl McpInvocationReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Unauthenticated => "unauthenticated",
            Self::InvalidCredential => "invalid_credential",
            Self::ToolNotAllowed => "tool_not_allowed",
            Self::AccessDenied => "access_denied",
            Self::NotFound => "not_found",
            Self::InvalidInput => "invalid_input",
            Self::Unavailable => "unavailable",
        }
    }
}

/// Opaque invocation authorization. Its fields cannot be supplied or changed
/// by an adapter; only `McpAuthenticatedClient` can construct one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpAuthorizedInvocation {
    request_id: String,
    client_id: String,
    purpose_code: String,
    actor: ActorRef,
    tool_id: McpToolId,
    registry_id: String,
    organization_id: String,
    context: AccessContext,
}

impl McpAuthorizedInvocation {
    /// The exact permission- and tenant-narrowed context that must be passed to
    /// the future application use case for this audited invocation.
    pub fn access_context(&self) -> &AccessContext {
        &self.context
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpInvocationAuditEvent {
    request_id: String,
    client_id: Option<String>,
    purpose_code: Option<String>,
    actor: Option<ActorRef>,
    tool_id: McpToolId,
    registry_id: Option<String>,
    organization_id: Option<String>,
    outcome: McpInvocationOutcome,
    reason: McpInvocationReason,
    result_count: u16,
}

impl McpInvocationAuditEvent {
    pub fn request_id(&self) -> &str {
        &self.request_id
    }
    pub fn client_id(&self) -> Option<&str> {
        self.client_id.as_deref()
    }
    pub fn purpose_code(&self) -> Option<&str> {
        self.purpose_code.as_deref()
    }
    pub fn actor(&self) -> Option<&ActorRef> {
        self.actor.as_ref()
    }
    pub const fn tool_id(&self) -> McpToolId {
        self.tool_id
    }
    pub fn registry_id(&self) -> Option<&str> {
        self.registry_id.as_deref()
    }
    pub fn organization_id(&self) -> Option<&str> {
        self.organization_id.as_deref()
    }
    pub const fn outcome(&self) -> McpInvocationOutcome {
        self.outcome
    }
    pub const fn reason(&self) -> McpInvocationReason {
        self.reason
    }
    pub const fn result_count(&self) -> u16 {
        self.result_count
    }

    pub fn validate(&self) -> bool {
        is_audit_request_reference(&self.request_id)
            && self.client_id.as_deref().is_none_or(valid_binding_code)
            && self.purpose_code.as_deref().is_none_or(valid_binding_code)
            && self.actor.as_ref().is_none_or(|actor| {
                actor.kind == ActorKind::IntegrationClient
                    && !actor.id.is_empty()
                    && actor.id.len() <= 512
                    && !actor.id.bytes().any(|byte| byte.is_ascii_control())
            })
            && self
                .registry_id
                .as_deref()
                .is_none_or(valid_scope_reference)
            && self
                .organization_id
                .as_deref()
                .is_none_or(valid_scope_reference)
            && (self.registry_id.is_some() == self.organization_id.is_some())
            && self.result_count <= MCP_MAX_AUDIT_RESULT_COUNT
            && match self.outcome {
                McpInvocationOutcome::Succeeded => {
                    self.reason == McpInvocationReason::None
                        && self.actor.is_some()
                        && self.client_id.is_some()
                        && self.purpose_code.is_some()
                        && self.registry_id.is_some()
                }
                McpInvocationOutcome::Denied => {
                    self.reason != McpInvocationReason::None
                        && self.result_count == 0
                        && self.actor.is_some() == self.client_id.is_some()
                        && self.client_id.is_some() == self.purpose_code.is_some()
                }
            }
    }
}

fn valid_scope_reference(value: &str) -> bool {
    (1..=128).contains(&value.len()) && value.bytes().all(|byte| byte.is_ascii_graphic())
}

fn is_audit_request_reference(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[derive(Debug, Error)]
#[error("MCP invocation audit unavailable")]
pub struct McpInvocationAuditError;

#[async_trait]
pub trait McpInvocationAudit: Send + Sync + 'static {
    async fn append(&self, event: &McpInvocationAuditEvent) -> Result<(), McpInvocationAuditError>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpInvocationDenial {
    Unauthenticated,
    InvalidCredential,
    ToolNotAllowed,
    AccessDenied,
    NotFound,
    InvalidInput,
    Unavailable,
}

impl McpInvocationDenial {
    const fn reason(self) -> McpInvocationReason {
        match self {
            Self::Unauthenticated => McpInvocationReason::Unauthenticated,
            Self::InvalidCredential => McpInvocationReason::InvalidCredential,
            Self::ToolNotAllowed => McpInvocationReason::ToolNotAllowed,
            Self::AccessDenied => McpInvocationReason::AccessDenied,
            Self::NotFound => McpInvocationReason::NotFound,
            Self::InvalidInput => McpInvocationReason::InvalidInput,
            Self::Unavailable => McpInvocationReason::Unavailable,
        }
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum McpInvocationFailure {
    #[error("MCP invocation audit unavailable")]
    AuditUnavailable,
    #[error("MCP request denied")]
    Denied(McpInvocationDenial),
}

/// Holds a prepared tool result until its success/denial event is durable.
pub struct McpInvocationBoundary {
    audit: Arc<dyn McpInvocationAudit>,
}

impl McpInvocationBoundary {
    pub fn new(audit: Arc<dyn McpInvocationAudit>) -> Self {
        Self { audit }
    }

    pub async fn before_reply<T>(
        &self,
        invocation: McpAuthorizedInvocation,
        prepared: Result<(T, u16), McpInvocationDenial>,
    ) -> Result<T, McpInvocationFailure> {
        let (outcome, reason, result_count) = match &prepared {
            Ok((_, result_count)) => (
                McpInvocationOutcome::Succeeded,
                McpInvocationReason::None,
                *result_count,
            ),
            Err(denial) => (McpInvocationOutcome::Denied, denial.reason(), 0),
        };
        let event = McpInvocationAuditEvent {
            request_id: invocation.request_id,
            client_id: Some(invocation.client_id),
            purpose_code: Some(invocation.purpose_code),
            actor: Some(invocation.actor),
            tool_id: invocation.tool_id,
            registry_id: Some(invocation.registry_id),
            organization_id: Some(invocation.organization_id),
            outcome,
            reason,
            result_count,
        };
        if !event.validate() {
            return Err(McpInvocationFailure::AuditUnavailable);
        }
        self.audit
            .append(&event)
            .await
            .map_err(|_| McpInvocationFailure::AuditUnavailable)?;
        prepared
            .map(|(result, _)| result)
            .map_err(McpInvocationFailure::Denied)
    }

    /// Records attempts rejected before authentication. The event deliberately
    /// carries no actor, client, purpose, or tenant attribution.
    pub async fn record_unattributed_denial(
        &self,
        request_id: String,
        tool_id: McpToolId,
        denial: McpInvocationDenial,
    ) -> Result<(), McpInvocationFailure> {
        if !valid_request_id(&request_id) {
            return Err(McpInvocationFailure::AuditUnavailable);
        }
        let event = McpInvocationAuditEvent {
            request_id: audit_request_reference(&request_id),
            client_id: None,
            purpose_code: None,
            actor: None,
            tool_id,
            registry_id: None,
            organization_id: None,
            outcome: McpInvocationOutcome::Denied,
            reason: denial.reason(),
            result_count: 0,
        };
        if !event.validate() {
            return Err(McpInvocationFailure::AuditUnavailable);
        }
        self.audit
            .append(&event)
            .await
            .map_err(|_| McpInvocationFailure::AuditUnavailable)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct SyntheticDirectory(AccessIdentity);

    #[async_trait]
    impl AccessDirectory for SyntheticDirectory {
        async fn load_identity(
            &self,
            kind: ActorKind,
            issuer_url: Option<&str>,
            external_subject: &str,
        ) -> Result<Option<AccessIdentity>, super::super::AccessDirectoryError> {
            Ok((self.0.actor.kind == kind
                && self.0.actor.issuer_url.as_deref() == issuer_url
                && self.0.actor.id == external_subject)
                .then(|| self.0.clone()))
        }
    }

    struct SyntheticCredentialStore {
        current: McpCredentialIdentity,
        issue_calls: AtomicUsize,
    }

    #[async_trait]
    impl McpCredentialStore for SyntheticCredentialStore {
        async fn issue(
            &self,
            _: &ActorRef,
            _: &McpCredentialProfile,
            _: &str,
        ) -> Result<String, SessionError> {
            self.issue_calls.fetch_add(1, Ordering::SeqCst);
            Ok("synthetic-opaque-mcp-secret".into())
        }

        async fn authenticate(
            &self,
            _: &str,
            _: &str,
        ) -> Result<McpCredentialIdentity, SessionError> {
            Ok(self.current.clone())
        }

        async fn revoke(&self, _: &str, _: &str) -> Result<bool, SessionError> {
            Ok(true)
        }
    }

    struct FailingAudit;

    #[async_trait]
    impl McpInvocationAudit for FailingAudit {
        async fn append(&self, _: &McpInvocationAuditEvent) -> Result<(), McpInvocationAuditError> {
            Err(McpInvocationAuditError)
        }
    }

    fn integration_identity(permissions: &[Permission]) -> AccessIdentity {
        AccessIdentity {
            actor: ActorRef {
                kind: ActorKind::IntegrationClient,
                id: "synthetic-client".into(),
                issuer_url: Some("https://issuer.example.test".into()),
            },
            active: true,
            grants: vec![super::super::AccessGrant {
                registry_id: "registry-synthetic".into(),
                organization_id: "org-synthetic".into(),
                record_ids: None,
                roles: vec![Role::IntegrationClient],
                permissions: permissions.to_vec(),
            }],
        }
    }

    fn profile(tools: &[McpToolId]) -> McpCredentialProfile {
        McpCredentialProfile::new(
            "client.reviewed-placeholder".into(),
            "purpose.reviewed-placeholder".into(),
            tools.iter().copied(),
            300,
        )
        .unwrap()
    }

    #[test]
    fn credential_profile_requires_bound_client_purpose_tools_and_short_ttl() {
        assert!(McpCredentialProfile::new("client".into(), "purpose".into(), [], 300).is_none());
        assert!(
            McpCredentialProfile::new(
                "client".into(),
                "purpose".into(),
                [McpToolId::RegistryGetRecordSummary],
                0,
            )
            .is_none()
        );
        assert!(
            McpCredentialProfile::new(
                "client".into(),
                "purpose".into(),
                [McpToolId::RegistryGetRecordSummary],
                MCP_CREDENTIAL_MAX_TTL_SECONDS + 1,
            )
            .is_none()
        );
    }

    #[tokio::test]
    async fn issue_rejects_tools_without_existing_integration_client_permission() {
        let identity = integration_identity(&[Permission::PersonRead]);
        let store = Arc::new(SyntheticCredentialStore {
            current: McpCredentialIdentity {
                identity: identity.clone(),
                binding: McpCredentialBinding {
                    client_id: "client".into(),
                    purpose_code: "purpose".into(),
                    allowed_tools: BTreeSet::from([McpToolId::RegistryGetRecordSummary]),
                },
            },
            issue_calls: AtomicUsize::new(0),
        });
        let service = McpAccessService::new(Arc::new(SyntheticDirectory(identity)), store.clone());
        assert!(matches!(
            service
                .issue(
                    Some("https://issuer.example.test"),
                    "synthetic-client",
                    &profile(&[McpToolId::RegistrySearchRecords]),
                    "request-1",
                )
                .await,
            Err(AccessServiceError::Rejected)
        ));
        assert_eq!(store.issue_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn authentication_reloads_grants_and_filters_revoked_tool_permissions() {
        let identity = integration_identity(&[Permission::PersonRead]);
        let store = Arc::new(SyntheticCredentialStore {
            current: McpCredentialIdentity {
                identity,
                binding: McpCredentialBinding {
                    client_id: "client.reviewed".into(),
                    purpose_code: "purpose.reviewed".into(),
                    allowed_tools: BTreeSet::from([
                        McpToolId::RegistryGetRecordSummary,
                        McpToolId::RegistrySearchRecords,
                    ]),
                },
            },
            issue_calls: AtomicUsize::new(0),
        });
        let service = McpAccessService::new(
            Arc::new(SyntheticDirectory(integration_identity(&[
                Permission::PersonRead,
            ]))),
            store,
        );
        let client = service
            .authenticate("synthetic-opaque-mcp-secret", "request-2")
            .await
            .unwrap();
        assert!(client.allows_tool(McpToolId::RegistryGetRecordSummary));
        assert!(!client.allows_tool(McpToolId::RegistrySearchRecords));
        let context = client
            .context_for_tool(McpToolId::RegistryGetRecordSummary)
            .unwrap();
        assert_eq!(context.purpose(), Some("purpose.reviewed"));
        assert!(context.grants().iter().all(|grant| {
            grant.permissions.contains(&Permission::PersonRead)
                && !grant.permissions.contains(&Permission::PersonSearch)
        }));
        assert!(
            client
                .context_for_tool(McpToolId::RegistrySearchRecords)
                .is_none()
        );
    }

    #[tokio::test]
    async fn tool_context_contains_only_the_permission_for_that_tool() {
        let identity = integration_identity(&[Permission::PersonRead, Permission::PersonSearch]);
        let store = Arc::new(SyntheticCredentialStore {
            current: McpCredentialIdentity {
                identity: identity.clone(),
                binding: McpCredentialBinding {
                    client_id: "client.reviewed".into(),
                    purpose_code: "purpose.reviewed".into(),
                    allowed_tools: BTreeSet::from([
                        McpToolId::RegistryGetRecordSummary,
                        McpToolId::RegistrySearchRecords,
                    ]),
                },
            },
            issue_calls: AtomicUsize::new(0),
        });
        let client = McpAccessService::new(Arc::new(SyntheticDirectory(identity)), store)
            .authenticate("synthetic-opaque-mcp-secret", "request-4")
            .await
            .unwrap();

        let summary = client
            .context_for_tool(McpToolId::RegistryGetRecordSummary)
            .unwrap();
        assert!(summary.grants().iter().all(|grant| {
            grant.permissions == BTreeSet::from([Permission::PersonRead]) && grant.roles.is_empty()
        }));
        let search = client
            .context_for_tool(McpToolId::RegistrySearchRecords)
            .unwrap();
        assert!(search.grants().iter().all(|grant| {
            grant.permissions == BTreeSet::from([Permission::PersonSearch])
                && grant.roles.is_empty()
        }));
    }

    #[tokio::test]
    async fn prepared_tool_output_is_suppressed_when_audit_persistence_fails() {
        let identity = integration_identity(&[Permission::PersonRead]);
        let store = Arc::new(SyntheticCredentialStore {
            current: McpCredentialIdentity {
                identity: identity.clone(),
                binding: McpCredentialBinding {
                    client_id: "client.reviewed".into(),
                    purpose_code: "purpose.reviewed".into(),
                    allowed_tools: BTreeSet::from([McpToolId::RegistryGetRecordSummary]),
                },
            },
            issue_calls: AtomicUsize::new(0),
        });
        let client = McpAccessService::new(Arc::new(SyntheticDirectory(identity)), store)
            .authenticate("synthetic-opaque-mcp-secret", "request-3-secret-canary")
            .await
            .unwrap();
        let invocation = client
            .invocation_for_tool(
                McpToolId::RegistryGetRecordSummary,
                "registry-synthetic",
                "org-synthetic",
            )
            .expect("authenticated tool and scope build opaque invocation");
        let boundary = McpInvocationBoundary::new(Arc::new(FailingAudit));
        let prepared_output = String::from("synthetic prepared summary");
        let result = boundary
            .before_reply(invocation, Ok((prepared_output, 1)))
            .await;
        assert_eq!(result, Err(McpInvocationFailure::AuditUnavailable));
    }

    #[tokio::test]
    async fn invocation_scope_is_derived_from_an_allowed_tool_and_current_grant() {
        let identity = integration_identity(&[Permission::PersonRead, Permission::PersonSearch]);
        let store = Arc::new(SyntheticCredentialStore {
            current: McpCredentialIdentity {
                identity: identity.clone(),
                binding: McpCredentialBinding {
                    client_id: "client.bound".into(),
                    purpose_code: "purpose.bound".into(),
                    allowed_tools: BTreeSet::from([
                        McpToolId::RegistryGetRecordSummary,
                        McpToolId::RegistrySearchRecords,
                    ]),
                },
            },
            issue_calls: AtomicUsize::new(0),
        });
        let client = McpAccessService::new(Arc::new(SyntheticDirectory(identity)), store)
            .authenticate("synthetic-opaque-mcp-secret", "caller-controlled-canary")
            .await
            .unwrap();

        assert!(
            client
                .invocation_for_tool(
                    McpToolId::RegistryGetRecordSummary,
                    "registry-other",
                    "org-synthetic",
                )
                .is_none()
        );
        assert!(
            client
                .invocation_for_tool(
                    McpToolId::MatchingRequestCandidates,
                    "registry-synthetic",
                    "org-synthetic",
                )
                .is_none()
        );
        let invocation = client
            .invocation_for_tool(
                McpToolId::RegistryGetRecordSummary,
                "registry-synthetic",
                "org-synthetic",
            )
            .unwrap();
        let audit = CaptureAudit::default();
        let boundary = McpInvocationBoundary::new(Arc::new(audit.clone()));
        boundary
            .before_reply(invocation, Ok(("safe", 1)))
            .await
            .unwrap();
        let event = audit.event.lock().unwrap().clone().unwrap();
        assert_ne!(event.request_id(), "caller-controlled-canary");
        assert_eq!(event.client_id(), Some("client.bound"));
        assert_eq!(event.purpose_code(), Some("purpose.bound"));
        assert_eq!(event.actor().unwrap().id, "synthetic-client");
        assert_eq!(event.tool_id(), McpToolId::RegistryGetRecordSummary);
        assert_eq!(event.registry_id(), Some("registry-synthetic"));
        assert_eq!(event.organization_id(), Some("org-synthetic"));
        assert_eq!(event.outcome(), McpInvocationOutcome::Succeeded);

        let denied_audit = CaptureAudit::default();
        let denied_boundary = McpInvocationBoundary::new(Arc::new(denied_audit.clone()));
        let denied_invocation = client
            .invocation_for_tool(
                McpToolId::RegistryGetRecordSummary,
                "registry-synthetic",
                "org-synthetic",
            )
            .unwrap();
        assert_eq!(
            denied_boundary
                .before_reply::<()>(denied_invocation, Err(McpInvocationDenial::AccessDenied),)
                .await,
            Err(McpInvocationFailure::Denied(
                McpInvocationDenial::AccessDenied
            ))
        );
        let denied_event = denied_audit.event.lock().unwrap().clone().unwrap();
        assert_eq!(denied_event.actor().unwrap().id, "synthetic-client");
        assert_eq!(denied_event.client_id(), Some("client.bound"));
        assert_eq!(denied_event.purpose_code(), Some("purpose.bound"));
        assert_eq!(denied_event.tool_id(), McpToolId::RegistryGetRecordSummary);
        assert_eq!(denied_event.reason(), McpInvocationReason::AccessDenied);
    }

    #[tokio::test]
    async fn invocation_context_and_audit_scope_are_the_same_for_multi_org_clients() {
        let mut identity = integration_identity(&[Permission::PersonRead]);
        identity.grants.push(super::super::AccessGrant {
            registry_id: "registry-synthetic".into(),
            organization_id: "org-other".into(),
            record_ids: None,
            roles: vec![Role::IntegrationClient],
            permissions: vec![Permission::PersonRead],
        });
        let store = Arc::new(SyntheticCredentialStore {
            current: McpCredentialIdentity {
                identity: identity.clone(),
                binding: McpCredentialBinding {
                    client_id: "client.multi-org".into(),
                    purpose_code: "purpose.bound".into(),
                    allowed_tools: BTreeSet::from([McpToolId::RegistryGetRecordSummary]),
                },
            },
            issue_calls: AtomicUsize::new(0),
        });
        let client = McpAccessService::new(Arc::new(SyntheticDirectory(identity)), store)
            .authenticate("synthetic-opaque-mcp-secret", "multi-org-request")
            .await
            .unwrap();
        let invocation = client
            .invocation_for_tool(
                McpToolId::RegistryGetRecordSummary,
                "registry-synthetic",
                "org-synthetic",
            )
            .unwrap();
        assert!(
            invocation
                .access_context()
                .grants()
                .iter()
                .all(|grant| grant.organization_id == "org-synthetic")
        );
        assert!(invocation.access_context().grants().iter().all(|grant| {
            grant.permissions == BTreeSet::from([Permission::PersonRead]) && grant.roles.is_empty()
        }));
        assert_eq!(invocation.registry_id, "registry-synthetic");
        assert_eq!(invocation.organization_id, "org-synthetic");
    }

    #[derive(Clone, Default)]
    struct CaptureAudit {
        event: Arc<std::sync::Mutex<Option<McpInvocationAuditEvent>>>,
    }

    #[async_trait]
    impl McpInvocationAudit for CaptureAudit {
        async fn append(
            &self,
            event: &McpInvocationAuditEvent,
        ) -> Result<(), McpInvocationAuditError> {
            *self.event.lock().unwrap() = Some(event.clone());
            Ok(())
        }
    }
}
