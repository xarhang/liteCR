use std::sync::Arc;

use super::{
    AccessAuthenticator, AccessContext, AccessDirectory, AccessSecurityAudit,
    AccessSessionRepository, ActorKind, ActorRef, LoginChallenge, LoginResult, OidcFlowSecrets,
    OidcFlowStore, OidcProvider, OidcProviderError, SecurityAuditEvent, SecurityEventReason,
    SecurityEventType, SecurityRouteFamily, SecurityScopeShape, SessionError,
};

#[derive(Debug, Clone, Copy)]
pub struct AccessServiceConfig {
    pub login_flow_lifetime_seconds: i64,
    pub session_lifetime_seconds: i64,
}

impl AccessServiceConfig {
    pub fn validate(self) -> Option<Self> {
        (self.login_flow_lifetime_seconds > 0
            && self.login_flow_lifetime_seconds <= 900
            && self.session_lifetime_seconds > 0
            && self.session_lifetime_seconds <= 86_400)
            .then_some(self)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AccessServiceError {
    #[error("access service unavailable")]
    Unavailable,
    #[error("authentication rejected")]
    Rejected,
    #[error("access is not provisioned")]
    NotProvisioned,
}

pub struct AccessService {
    provider: Arc<dyn OidcProvider>,
    flows: Arc<dyn OidcFlowStore>,
    sessions: Arc<dyn AccessSessionRepository>,
    directory: Arc<dyn AccessDirectory>,
    authenticator: Arc<dyn AccessAuthenticator>,
    security_audit: Arc<dyn AccessSecurityAudit>,
    config: AccessServiceConfig,
}

impl AccessService {
    pub fn new(
        provider: Arc<dyn OidcProvider>,
        flows: Arc<dyn OidcFlowStore>,
        sessions: Arc<dyn AccessSessionRepository>,
        directory: Arc<dyn AccessDirectory>,
        authenticator: Arc<dyn AccessAuthenticator>,
        security_audit: Arc<dyn AccessSecurityAudit>,
        config: AccessServiceConfig,
    ) -> Option<Self> {
        Some(Self {
            provider,
            flows,
            sessions,
            directory,
            authenticator,
            security_audit,
            config: config.validate()?,
        })
    }

    pub async fn begin_login(&self) -> Result<LoginChallenge, AccessServiceError> {
        let authorization = self
            .provider
            .begin_authorization()
            .await
            .map_err(map_provider_error)?;
        self.flows
            .create(
                &OidcFlowSecrets {
                    flow_id: authorization.flow_id.clone(),
                    state: authorization.state,
                    nonce: authorization.nonce,
                    pkce_verifier: authorization.pkce_verifier,
                    redirect_uri: authorization.redirect_uri,
                },
                self.config.login_flow_lifetime_seconds,
            )
            .await
            .map_err(|_| AccessServiceError::Unavailable)?;
        Ok(LoginChallenge {
            authorization_url: authorization.authorization_url,
            flow_cookie: authorization.flow_id,
        })
    }

    pub async fn complete_login(
        &self,
        flow_cookie: &str,
        state: &str,
        code: &str,
        request_id: &str,
    ) -> Result<LoginResult, AccessServiceError> {
        let flow = self
            .flows
            .consume(flow_cookie, state)
            .await
            .map_err(|_| AccessServiceError::Unavailable)?;
        let Some(flow) = flow else {
            self.record_security_event(SecurityAuditEvent {
                event_type: SecurityEventType::OidcCallbackDenied,
                reason: SecurityEventReason::InvalidLoginFlow,
                route_family: SecurityRouteFamily::OidcCallback,
                scope_shape: SecurityScopeShape::None,
                actor: None,
                request_id: request_id.to_owned(),
            })
            .await?;
            return Err(AccessServiceError::Rejected);
        };
        let claims = self
            .provider
            .exchange_code(code, &flow)
            .await
            .map_err(|error| match error {
                OidcProviderError::Unavailable => AccessServiceError::Unavailable,
                OidcProviderError::InvalidResponse | OidcProviderError::Rejected => {
                    AccessServiceError::Rejected
                }
            });
        let claims = match claims {
            Ok(claims) => claims,
            Err(AccessServiceError::Rejected) => {
                self.record_security_event(SecurityAuditEvent {
                    event_type: SecurityEventType::OidcCallbackDenied,
                    reason: SecurityEventReason::IdentityProviderRejected,
                    route_family: SecurityRouteFamily::OidcCallback,
                    scope_shape: SecurityScopeShape::None,
                    actor: None,
                    request_id: request_id.to_owned(),
                })
                .await?;
                return Err(AccessServiceError::Rejected);
            }
            Err(error) => return Err(error),
        };
        if claims.subject.is_empty()
            || claims.issuer.is_empty()
            || claims.audience.is_empty()
            || claims.nonce != flow.authorization.nonce
            || claims.expires_at_epoch_seconds <= current_epoch_seconds()
        {
            self.record_security_event(SecurityAuditEvent {
                event_type: SecurityEventType::OidcCallbackDenied,
                reason: SecurityEventReason::InvalidIdentity,
                route_family: SecurityRouteFamily::OidcCallback,
                scope_shape: SecurityScopeShape::None,
                actor: None,
                request_id: request_id.to_owned(),
            })
            .await?;
            return Err(AccessServiceError::Rejected);
        }
        let identity = self
            .directory
            .load_identity(ActorKind::Human, Some(&claims.issuer), &claims.subject)
            .await
            .map_err(|_| AccessServiceError::Unavailable)?;
        let Some(identity) =
            identity.filter(|identity| identity.active && !identity.grants.is_empty())
        else {
            self.record_security_event(SecurityAuditEvent {
                event_type: SecurityEventType::OidcCallbackDenied,
                reason: SecurityEventReason::IdentityNotProvisioned,
                route_family: SecurityRouteFamily::OidcCallback,
                scope_shape: SecurityScopeShape::None,
                actor: None,
                request_id: request_id.to_owned(),
            })
            .await?;
            return Err(AccessServiceError::NotProvisioned);
        };
        let actor = ActorRef {
            kind: ActorKind::Human,
            id: identity.actor.id,
            issuer_url: Some(claims.issuer),
        };
        let session = self
            .sessions
            .create(
                &actor,
                claims.session_id.as_deref(),
                self.config.session_lifetime_seconds,
                request_id,
            )
            .await
            .map_err(|_| AccessServiceError::Unavailable)?;
        Ok(LoginResult { session, actor })
    }

    pub async fn record_security_event(
        &self,
        event: SecurityAuditEvent,
    ) -> Result<(), AccessServiceError> {
        if !event.validate() {
            return Err(AccessServiceError::Unavailable);
        }
        self.security_audit
            .append(&event)
            .await
            .map_err(|_| AccessServiceError::Unavailable)
    }

    pub async fn authenticate(
        &self,
        session_cookie: &str,
        csrf_token: Option<&str>,
        require_csrf: bool,
        request_id: &str,
    ) -> Result<AccessContext, SessionError> {
        let identity = self
            .authenticator
            .authenticate_cookie(session_cookie, csrf_token, require_csrf, request_id)
            .await?;
        let actor = identity.actor.clone();
        AccessContext::from_authorized_identity(
            actor,
            identity.into_scope_grants(),
            request_id.to_owned(),
            None,
            None,
        )
        .ok_or(SessionError::Invalid)
    }

    pub async fn logout(&self, session_cookie: &str, request_id: &str) -> Result<(), SessionError> {
        self.sessions.revoke(session_cookie, request_id).await
    }

    pub async fn revoke_actor_sessions(
        &self,
        actor: &ActorRef,
        request_id: &str,
    ) -> Result<u64, SessionError> {
        self.sessions.revoke_actor_sessions(actor, request_id).await
    }

    pub async fn authenticate_integration_credential(
        &self,
        credential: &str,
        request_id: &str,
    ) -> Result<AccessContext, SessionError> {
        let identity = self
            .authenticator
            .authenticate_integration_credential(credential, request_id)
            .await?;
        let actor = identity.actor.clone();
        AccessContext::from_authorized_identity(
            actor,
            identity.into_scope_grants(),
            request_id.to_owned(),
            None,
            None,
        )
        .ok_or(SessionError::Invalid)
    }
}

fn map_provider_error(error: OidcProviderError) -> AccessServiceError {
    match error {
        OidcProviderError::Unavailable => AccessServiceError::Unavailable,
        OidcProviderError::InvalidResponse | OidcProviderError::Rejected => {
            AccessServiceError::Rejected
        }
    }
}

fn current_epoch_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::access::{
        AccessGrant, AccessIdentity, AccessSession, AccessSessionRepository, OidcAuthorization,
        OidcFlow, OidcIdentityClaims, OidcProvider, OidcProviderError, Permission, Role,
        SessionCredentials,
    };
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct SyntheticProvider {
        claims: Result<OidcIdentityClaims, ()>,
    }

    #[async_trait]
    impl OidcProvider for SyntheticProvider {
        async fn begin_authorization(&self) -> Result<OidcAuthorization, OidcProviderError> {
            Ok(authorization())
        }

        async fn exchange_code(
            &self,
            _code: &str,
            _flow: &OidcFlow,
        ) -> Result<OidcIdentityClaims, OidcProviderError> {
            self.claims
                .clone()
                .map_err(|()| OidcProviderError::Rejected)
        }
    }

    struct SyntheticFlows;

    #[async_trait]
    impl OidcFlowStore for SyntheticFlows {
        async fn create(
            &self,
            _flow: &OidcFlowSecrets,
            _expires_in_seconds: i64,
        ) -> Result<(), SessionError> {
            Ok(())
        }

        async fn consume(
            &self,
            flow_id: &str,
            state: &str,
        ) -> Result<Option<OidcFlow>, SessionError> {
            let expected = authorization();
            Ok(
                (flow_id == expected.flow_id && state == expected.state).then_some(OidcFlow {
                    flow_id: expected.flow_id.clone(),
                    authorization: expected,
                }),
            )
        }
    }

    struct SyntheticDirectory {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl AccessDirectory for SyntheticDirectory {
        async fn load_identity(
            &self,
            kind: ActorKind,
            issuer_url: Option<&str>,
            external_subject: &str,
        ) -> Result<Option<AccessIdentity>, crate::access::AccessDirectoryError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(Some(AccessIdentity {
                actor: ActorRef {
                    kind,
                    id: external_subject.to_owned(),
                    issuer_url: issuer_url.map(str::to_owned),
                },
                active: true,
                grants: vec![AccessGrant {
                    registry_id: "registry-synthetic".to_owned(),
                    organization_id: "org-synthetic".to_owned(),
                    record_ids: None,
                    roles: vec![Role::Registrar],
                    permissions: vec![Permission::PersonRead],
                }],
            }))
        }
    }

    struct SyntheticSessions {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl AccessSessionRepository for SyntheticSessions {
        async fn create(
            &self,
            _actor: &ActorRef,
            _oidc_session_id: Option<&str>,
            _expires_in_seconds: i64,
            _request_id: &str,
        ) -> Result<SessionCredentials, SessionError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(SessionCredentials {
                opaque_token: "synthetic-session-token-with-sufficient-entropy".to_owned(),
                csrf_token: "synthetic-csrf-token-with-sufficient-entropy".to_owned(),
            })
        }

        async fn resolve(&self, _token: &str) -> Result<Option<AccessSession>, SessionError> {
            Ok(None)
        }

        async fn verify_csrf(&self, _session_id: &str, _token: &str) -> Result<bool, SessionError> {
            Ok(false)
        }

        async fn revoke(&self, _token: &str, _request_id: &str) -> Result<(), SessionError> {
            Ok(())
        }

        async fn revoke_actor_sessions(
            &self,
            _actor: &ActorRef,
            _request_id: &str,
        ) -> Result<u64, SessionError> {
            Ok(0)
        }
    }

    struct SyntheticAuthenticator;

    #[async_trait]
    impl AccessAuthenticator for SyntheticAuthenticator {
        async fn authenticate_cookie(
            &self,
            _token: &str,
            _csrf_token: Option<&str>,
            _require_csrf: bool,
            _request_id: &str,
        ) -> Result<AccessIdentity, SessionError> {
            Err(SessionError::Invalid)
        }

        async fn authenticate_integration_credential(
            &self,
            _credential: &str,
            _request_id: &str,
        ) -> Result<AccessIdentity, SessionError> {
            Err(SessionError::Invalid)
        }
    }

    fn authorization() -> OidcAuthorization {
        OidcAuthorization {
            flow_id: "synthetic-flow-id".to_owned(),
            authorization_url: "https://idp.example.test/authorize".to_owned(),
            redirect_uri: "https://litecr.example.test/callback".to_owned(),
            state: "synthetic-state".to_owned(),
            nonce: "synthetic-nonce".to_owned(),
            pkce_verifier: "synthetic-pkce-verifier".to_owned(),
        }
    }

    fn claims() -> OidcIdentityClaims {
        OidcIdentityClaims {
            issuer: "https://idp.example.test".to_owned(),
            subject: "synthetic-user".to_owned(),
            audience: "litecr-web".to_owned(),
            expires_at_epoch_seconds: i64::MAX,
            nonce: "synthetic-nonce".to_owned(),
            session_id: None,
            email_verified: None,
        }
    }

    fn service(
        provider_result: Result<OidcIdentityClaims, ()>,
    ) -> (
        AccessService,
        Arc<SyntheticDirectory>,
        Arc<SyntheticSessions>,
    ) {
        let directory = Arc::new(SyntheticDirectory {
            calls: AtomicUsize::new(0),
        });
        let sessions = Arc::new(SyntheticSessions {
            calls: AtomicUsize::new(0),
        });
        let service = AccessService::new(
            Arc::new(SyntheticProvider {
                claims: provider_result,
            }),
            Arc::new(SyntheticFlows),
            sessions.clone(),
            directory.clone(),
            Arc::new(SyntheticAuthenticator),
            Arc::new(crate::access::NoopAccessSecurityAudit),
            AccessServiceConfig {
                login_flow_lifetime_seconds: 300,
                session_lifetime_seconds: 3_600,
            },
        )
        .unwrap();
        (service, directory, sessions)
    }

    async fn complete(
        service: &AccessService,
    ) -> Result<crate::access::LoginResult, AccessServiceError> {
        service
            .complete_login(
                "synthetic-flow-id",
                "synthetic-state",
                "synthetic-code",
                "request-synthetic",
            )
            .await
    }

    #[test]
    fn session_and_flow_lifetimes_are_bounded() {
        assert!(
            AccessServiceConfig {
                login_flow_lifetime_seconds: 300,
                session_lifetime_seconds: 28_800,
            }
            .validate()
            .is_some()
        );
        assert!(
            AccessServiceConfig {
                login_flow_lifetime_seconds: 901,
                session_lifetime_seconds: 28_800,
            }
            .validate()
            .is_none()
        );
        assert!(
            AccessServiceConfig {
                login_flow_lifetime_seconds: 300,
                session_lifetime_seconds: 86_401,
            }
            .validate()
            .is_none()
        );
    }

    #[tokio::test]
    async fn login_rejects_missing_identity_claims_expired_token_and_wrong_nonce_before_provisioning()
     {
        let mut invalid_claim_sets = Vec::new();
        let mut value = claims();
        value.subject.clear();
        invalid_claim_sets.push(("empty subject", value));
        let mut value = claims();
        value.issuer.clear();
        invalid_claim_sets.push(("empty issuer", value));
        let mut value = claims();
        value.audience.clear();
        invalid_claim_sets.push(("empty audience", value));
        let mut value = claims();
        value.expires_at_epoch_seconds = 1;
        invalid_claim_sets.push(("expired token", value));
        let mut value = claims();
        value.nonce = "different-synthetic-nonce".to_owned();
        invalid_claim_sets.push(("nonce mismatch", value));

        for (label, bad_claims) in invalid_claim_sets {
            let (service, directory, sessions) = service(Ok(bad_claims));
            assert!(
                matches!(complete(&service).await, Err(AccessServiceError::Rejected)),
                "{label}"
            );
            assert_eq!(
                directory.calls.load(Ordering::SeqCst),
                0,
                "{label} must fail before identity lookup"
            );
            assert_eq!(
                sessions.calls.load(Ordering::SeqCst),
                0,
                "{label} must fail before issuing a session"
            );
        }
    }

    #[tokio::test]
    async fn rejected_provider_token_does_not_lookup_identity_or_issue_session() {
        // The provider returns Rejected for cryptographic or protocol validation failures,
        // including an invalid signature. The service must fail closed on that boundary.
        let (service, directory, sessions) = service(Err(()));
        assert!(matches!(
            complete(&service).await,
            Err(AccessServiceError::Rejected)
        ));
        assert_eq!(directory.calls.load(Ordering::SeqCst), 0);
        assert_eq!(sessions.calls.load(Ordering::SeqCst), 0);
    }
}
