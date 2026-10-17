//! Framework-independent access context and authorization contracts.

mod context;
mod mcp;
mod policy;
mod ports;
mod service;

pub use context::{
    AccessContext, ActorKind, ActorRef, Permission, ResourceScope, Role, ScopeGrant,
};
pub use mcp::{
    MCP_CREDENTIAL_MAX_TTL_SECONDS, MCP_MAX_AUDIT_RESULT_COUNT, MCP_STDIO_AUDIENCE,
    McpAccessService, McpAuthenticatedClient, McpAuthorizedInvocation, McpCredentialBinding,
    McpCredentialIdentity, McpCredentialProfile, McpCredentialStore, McpInvocationAudit,
    McpInvocationAuditError, McpInvocationAuditEvent, McpInvocationBoundary, McpInvocationDenial,
    McpInvocationFailure, McpInvocationOutcome, McpInvocationReason, McpToolId,
};
pub use policy::{AccessDenied, AccessError, AccessPolicy, authorize};
pub use ports::{
    AccessAuthenticator, AccessDirectory, AccessDirectoryError, AccessGrant, AccessIdentity,
    AccessSecurityAudit, AccessSession, AccessSessionRepository, LoginChallenge, LoginResult,
    NoopAccessSecurityAudit, OidcAuthorization, OidcFlow, OidcFlowSecrets, OidcFlowStore,
    OidcIdentityClaims, OidcProvider, OidcProviderError, SecurityAuditError, SecurityAuditEvent,
    SecurityEventReason, SecurityEventType, SecurityRouteFamily, SecurityScopeShape,
    SessionCredentials, SessionError,
};
pub use service::{AccessService, AccessServiceConfig, AccessServiceError};
