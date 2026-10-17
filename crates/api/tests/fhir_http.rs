use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
    routing::get,
};
use litecr_api::{
    access::{AccessApiConfig, AccessEnvironment, protected_routes},
    integrations::fhir::{self, FhirClientBinding},
};
use litecr_application::{
    access::{
        AccessAuthenticator, AccessDirectory, AccessDirectoryError, AccessGrant, AccessIdentity,
        AccessSecurityAudit, AccessService, AccessServiceConfig, AccessSession,
        AccessSessionRepository, ActorKind, ActorRef, OidcAuthorization, OidcFlow, OidcFlowSecrets,
        OidcFlowStore, OidcIdentityClaims, OidcProvider, OidcProviderError, Permission, Role,
        SecurityAuditError, SecurityAuditEvent, SecurityEventReason, SecurityEventType,
        SecurityRouteFamily, SecurityScopeShape, SessionCredentials, SessionError,
    },
    registry::{
        IngestionOutcome, IngestionReceipt, PersonView, RegistryService, RegistryStore,
        RegistryStoreError, SourceRecordCommand,
    },
};
use litecr_domain::registry::{IdentifierView, SourceAssertionView};
use serde_json::Value;
use tower::ServiceExt;

const ISSUER: &str = "https://issuer.example";
const REGISTRY: &str = "registry-synthetic";
const ORGANIZATION: &str = "org-east";

#[derive(Default)]
struct StoreProbe {
    person_lookups: Mutex<Vec<(String, String, String)>>,
    searches: Mutex<Vec<(String, String, String, String)>>,
    value_searches: Mutex<Vec<(String, String, String)>>,
    ingestions: Mutex<Vec<(String, SourceRecordCommand)>>,
    protected_handler_calls: Mutex<Vec<()>>,
}

struct StubRegistry(Arc<StoreProbe>);

#[async_trait]
impl RegistryStore for StubRegistry {
    async fn ingest(
        &self,
        _actor: &litecr_application::registry::RegistryActor,
        idempotency_key: &str,
        command: &SourceRecordCommand,
    ) -> Result<IngestionReceipt, RegistryStoreError> {
        let mut ingestions = self.0.ingestions.lock().unwrap();
        if let Some((_, previous_command)) = ingestions
            .iter()
            .find(|(previous_key, _)| previous_key == idempotency_key)
        {
            if previous_command != command {
                return Err(RegistryStoreError::IdempotencyKeyReused);
            }
            return Ok(synthetic_ingestion_receipt(true));
        }
        ingestions.push((idempotency_key.to_owned(), command.clone()));
        Ok(synthetic_ingestion_receipt(false))
    }

    async fn person(
        &self,
        registry_id: &str,
        organization_id: &str,
        public_id_or_alias: &str,
    ) -> Result<Option<PersonView>, RegistryStoreError> {
        self.0.person_lookups.lock().unwrap().push((
            registry_id.to_owned(),
            organization_id.to_owned(),
            public_id_or_alias.to_owned(),
        ));
        if registry_id == REGISTRY
            && organization_id == ORGANIZATION
            && public_id_or_alias == "litecr-synthetic-1"
        {
            Ok(Some(synthetic_person()))
        } else {
            Ok(None)
        }
    }

    async fn person_for_source(
        &self,
        _registry_id: &str,
        _organization_id: &str,
        _source_record_id: &str,
    ) -> Result<Option<PersonView>, RegistryStoreError> {
        Ok(None)
    }

    async fn search_public_id(
        &self,
        _registry_id: &str,
        _organization_id: &str,
        _public_id_or_alias: &str,
        _limit: u16,
    ) -> Result<Vec<PersonView>, RegistryStoreError> {
        Ok(Vec::new())
    }

    async fn search_identifier(
        &self,
        registry_id: &str,
        organization_id: &str,
        authority: &str,
        value: &str,
        _limit: u16,
    ) -> Result<Vec<PersonView>, RegistryStoreError> {
        self.0.searches.lock().unwrap().push((
            registry_id.to_owned(),
            organization_id.to_owned(),
            authority.to_owned(),
            value.to_owned(),
        ));
        Ok(Vec::new())
    }

    async fn search_identifier_value(
        &self,
        registry_id: &str,
        organization_id: &str,
        value: &str,
        _limit: u16,
    ) -> Result<Vec<PersonView>, RegistryStoreError> {
        self.0.value_searches.lock().unwrap().push((
            registry_id.to_owned(),
            organization_id.to_owned(),
            value.to_owned(),
        ));
        Ok(Vec::new())
    }
}

fn synthetic_ingestion_receipt(replayed: bool) -> IngestionReceipt {
    IngestionReceipt {
        person_public_id: "litecr-synthetic-1".to_owned(),
        source_record_id: "source-synthetic-1".to_owned(),
        source_receive_version: 1,
        outcome: IngestionOutcome::Accepted,
        replayed,
    }
}

struct SyntheticAuthenticator;

#[async_trait]
impl AccessAuthenticator for SyntheticAuthenticator {
    async fn authenticate_cookie(
        &self,
        _opaque_session_token: &str,
        _csrf_token: Option<&str>,
        _require_csrf: bool,
        _request_id: &str,
    ) -> Result<AccessIdentity, SessionError> {
        Err(SessionError::Invalid)
    }

    async fn authenticate_integration_credential(
        &self,
        opaque_credential: &str,
        _request_id: &str,
    ) -> Result<AccessIdentity, SessionError> {
        match opaque_credential {
            "synthetic-east-credential" => Ok(identity("client-east", ORGANIZATION)),
            "synthetic-west-credential" => Ok(identity("client-west", "org-west")),
            "synthetic-read-credential" => Ok(identity_with_permissions(
                "client-read",
                ORGANIZATION,
                vec![Permission::PersonRead],
            )),
            "synthetic-search-credential" => Ok(identity_with_permissions(
                "client-search",
                ORGANIZATION,
                vec![Permission::PersonSearch],
            )),
            "synthetic-write-credential" => Ok(identity_with_permissions(
                "client-write",
                ORGANIZATION,
                vec![Permission::SourceWrite],
            )),
            "synthetic-disabled-create-credential" => {
                Ok(identity("client-disabled-create", ORGANIZATION))
            }
            _ => Err(SessionError::Invalid),
        }
    }
}

struct UnusedDirectory;

#[async_trait]
impl AccessDirectory for UnusedDirectory {
    async fn load_identity(
        &self,
        _kind: ActorKind,
        _issuer_url: Option<&str>,
        _external_subject: &str,
    ) -> Result<Option<AccessIdentity>, AccessDirectoryError> {
        Ok(None)
    }
}

struct UnusedSessions;

#[async_trait]
impl AccessSessionRepository for UnusedSessions {
    async fn create(
        &self,
        _actor: &ActorRef,
        _oidc_session_id: Option<&str>,
        _expires_in_seconds: i64,
        _request_id: &str,
    ) -> Result<SessionCredentials, SessionError> {
        Err(SessionError::Unavailable)
    }

    async fn resolve(
        &self,
        _opaque_session_token: &str,
    ) -> Result<Option<AccessSession>, SessionError> {
        Err(SessionError::Unavailable)
    }

    async fn verify_csrf(
        &self,
        _session_id: &str,
        _csrf_token: &str,
    ) -> Result<bool, SessionError> {
        Err(SessionError::Unavailable)
    }

    async fn revoke(
        &self,
        _opaque_session_token: &str,
        _request_id: &str,
    ) -> Result<(), SessionError> {
        Err(SessionError::Unavailable)
    }

    async fn revoke_actor_sessions(
        &self,
        _actor: &ActorRef,
        _request_id: &str,
    ) -> Result<u64, SessionError> {
        Err(SessionError::Unavailable)
    }
}

struct UnusedProvider;

#[async_trait]
impl OidcProvider for UnusedProvider {
    async fn begin_authorization(&self) -> Result<OidcAuthorization, OidcProviderError> {
        Err(OidcProviderError::Unavailable)
    }

    async fn exchange_code(
        &self,
        _code: &str,
        _flow: &OidcFlow,
    ) -> Result<OidcIdentityClaims, OidcProviderError> {
        Err(OidcProviderError::Unavailable)
    }
}

struct UnusedFlows;

#[async_trait]
impl OidcFlowStore for UnusedFlows {
    async fn create(
        &self,
        _flow: &OidcFlowSecrets,
        _expires_in_seconds: i64,
    ) -> Result<(), SessionError> {
        Err(SessionError::Unavailable)
    }

    async fn consume(
        &self,
        _flow_id: &str,
        _state: &str,
    ) -> Result<Option<OidcFlow>, SessionError> {
        Ok(None)
    }
}

fn identity(subject: &str, organization_id: &str) -> AccessIdentity {
    identity_with_permissions(
        subject,
        organization_id,
        vec![
            Permission::PersonRead,
            Permission::PersonSearch,
            Permission::SourceWrite,
        ],
    )
}

fn identity_with_permissions(
    subject: &str,
    organization_id: &str,
    permissions: Vec<Permission>,
) -> AccessIdentity {
    AccessIdentity {
        actor: ActorRef {
            kind: ActorKind::IntegrationClient,
            id: subject.to_owned(),
            issuer_url: Some(ISSUER.to_owned()),
        },
        active: true,
        grants: vec![AccessGrant {
            registry_id: REGISTRY.to_owned(),
            organization_id: organization_id.to_owned(),
            record_ids: None,
            roles: vec![Role::IntegrationClient],
            permissions,
        }],
    }
}

#[derive(Default)]
struct RecordingSecurityAudit {
    events: Mutex<Vec<SecurityAuditEvent>>,
}

#[async_trait]
impl AccessSecurityAudit for RecordingSecurityAudit {
    async fn append(&self, event: &SecurityAuditEvent) -> Result<(), SecurityAuditError> {
        self.events.lock().unwrap().push(event.clone());
        Ok(())
    }
}

#[derive(Default)]
struct FailingSecurityAudit {
    attempts: Mutex<Vec<SecurityAuditEvent>>,
}

#[async_trait]
impl AccessSecurityAudit for FailingSecurityAudit {
    async fn append(&self, event: &SecurityAuditEvent) -> Result<(), SecurityAuditError> {
        self.attempts.lock().unwrap().push(event.clone());
        Err(SecurityAuditError)
    }
}

fn app() -> (Router, Arc<StoreProbe>) {
    app_with_security_audit(Arc::new(
        litecr_application::access::NoopAccessSecurityAudit,
    ))
}

fn app_with_security_audit(
    security_audit: Arc<dyn AccessSecurityAudit>,
) -> (Router, Arc<StoreProbe>) {
    let probe = Arc::new(StoreProbe::default());
    let registry =
        Arc::new(RegistryService::new(Box::new(StubRegistry(probe.clone())), 100).unwrap());
    let access = Arc::new(
        AccessService::new(
            Arc::new(UnusedProvider),
            Arc::new(UnusedFlows),
            Arc::new(UnusedSessions),
            Arc::new(UnusedDirectory),
            Arc::new(SyntheticAuthenticator),
            security_audit,
            AccessServiceConfig {
                login_flow_lifetime_seconds: 300,
                session_lifetime_seconds: 3600,
            },
        )
        .unwrap(),
    );
    let bindings = HashMap::from([
        (
            format!("{ISSUER}#client-east"),
            FhirClientBinding {
                registry_id: REGISTRY.to_owned(),
                organization_id: ORGANIZATION.to_owned(),
                source_system_id: "source-east".to_owned(),
                create_enabled: true,
            },
        ),
        (
            format!("{ISSUER}#client-west"),
            FhirClientBinding {
                registry_id: REGISTRY.to_owned(),
                organization_id: ORGANIZATION.to_owned(),
                source_system_id: "source-east".to_owned(),
                create_enabled: true,
            },
        ),
        (
            format!("{ISSUER}#client-read"),
            FhirClientBinding {
                registry_id: REGISTRY.to_owned(),
                organization_id: ORGANIZATION.to_owned(),
                source_system_id: "source-east".to_owned(),
                create_enabled: true,
            },
        ),
        (
            format!("{ISSUER}#client-search"),
            FhirClientBinding {
                registry_id: REGISTRY.to_owned(),
                organization_id: ORGANIZATION.to_owned(),
                source_system_id: "source-east".to_owned(),
                create_enabled: true,
            },
        ),
        (
            format!("{ISSUER}#client-write"),
            FhirClientBinding {
                registry_id: REGISTRY.to_owned(),
                organization_id: ORGANIZATION.to_owned(),
                source_system_id: "source-east".to_owned(),
                create_enabled: true,
            },
        ),
        (
            format!("{ISSUER}#client-disabled-create"),
            FhirClientBinding {
                registry_id: REGISTRY.to_owned(),
                organization_id: ORGANIZATION.to_owned(),
                source_system_id: "source-east".to_owned(),
                create_enabled: false,
            },
        ),
    ]);
    let protected_handler_probe = probe.clone();
    let routes = Router::new().merge(fhir::router(registry, bindings)).route(
        "/api/v1/test/protected",
        get(move || {
            let probe = protected_handler_probe.clone();
            async move {
                probe.protected_handler_calls.lock().unwrap().push(());
                StatusCode::OK
            }
        }),
    );
    let config = AccessApiConfig {
        expected_origin: "http://localhost".to_owned(),
        secure_cookies: false,
        environment: AccessEnvironment::Development,
        session_lifetime_seconds: 3600,
        flow_lifetime_seconds: 300,
    };
    let protected = protected_routes(routes, access.clone(), config.clone())
        .merge(litecr_api::access::router(access, config));
    (protected, probe)
}

#[tokio::test]
async fn duplicate_authorization_fields_are_rejected_before_authentication() {
    let audit = Arc::new(RecordingSecurityAudit::default());
    let (router, _) = app_with_security_audit(audit.clone());
    let mut request = Request::builder()
        .uri("/api/v1/test/protected")
        .header(header::AUTHORIZATION, "Bearer synthetic-east-credential")
        .body(Body::empty())
        .unwrap();
    request.headers_mut().append(
        header::AUTHORIZATION,
        "Bearer synthetic-west-credential".parse().unwrap(),
    );

    let response = router.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");

    let events = audit.events.lock().unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0].event_type,
        SecurityEventType::ProtectedRouteDenied
    );
    assert_eq!(
        events[0].reason,
        SecurityEventReason::ConflictingCredentials
    );
    assert_eq!(events[0].route_family, SecurityRouteFamily::OtherProtected);
    assert!(events[0].actor.is_none());
    let serialized = format!("{events:?}");
    assert!(!serialized.contains("synthetic-east-credential"));
    assert!(!serialized.contains("synthetic-west-credential"));
}

#[tokio::test]
async fn protected_request_fails_closed_when_security_denial_audit_is_unavailable() {
    let audit = Arc::new(FailingSecurityAudit::default());
    let (router, probe) = app_with_security_audit(audit.clone());
    let mut request = Request::builder()
        .uri("/api/v1/test/protected?token=query-token-canary")
        .header(header::AUTHORIZATION, "Bearer synthetic-east-credential")
        .body(Body::empty())
        .unwrap();
    request.headers_mut().append(
        header::AUTHORIZATION,
        "Bearer synthetic-west-credential".parse().unwrap(),
    );

    let response = router.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    let body = String::from_utf8(
        to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    assert!(!body.contains("query-token-canary"));
    assert!(!body.contains("synthetic-east-credential"));
    assert!(!body.contains("synthetic-west-credential"));

    let attempts = audit.attempts.lock().unwrap();
    assert_eq!(attempts.len(), 1);
    assert_eq!(
        attempts[0].event_type,
        SecurityEventType::ProtectedRouteDenied
    );
    assert_eq!(
        attempts[0].reason,
        SecurityEventReason::ConflictingCredentials
    );
    let serialized = format!("{attempts:?}");
    assert!(!serialized.contains("query-token-canary"));
    assert!(!serialized.contains("synthetic-east-credential"));
    assert!(!serialized.contains("synthetic-west-credential"));
    drop(attempts);

    assert!(probe.protected_handler_calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn rejected_callback_and_protected_requests_record_only_bounded_security_metadata() {
    let audit = Arc::new(RecordingSecurityAudit::default());
    let (router, _) = app_with_security_audit(audit.clone());

    let protected = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/fhir/Patient/record-canary?code=query-code-canary&token=query-token-canary")
                .header("x-request-id", "00000000-0000-4000-8000-000000000001")
                .header(
                    header::AUTHORIZATION,
                    "Bearer bearer-token-canary-not-for-audit",
                )
                .header(
                    header::COOKIE,
                    "litecr_session=session-cookie-canary; litecr_csrf=csrf-cookie-canary",
                )
                .body(Body::from("body-canary-not-for-audit"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(protected.status(), StatusCode::UNAUTHORIZED);

    let callback = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/access/oidc/callback?code=oidc-code-canary&state=oidc-state-canary&token=query-token-canary")
                .header("x-request-id", "00000000-0000-4000-8000-000000000002")
                .header(header::COOKIE, "litecr_oidc_flow=flow-cookie-canary")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(callback.status(), StatusCode::UNAUTHORIZED);

    let mut scoped_denial = request(
        "/fhir/r4/Patient?identifier=https%3A%2F%2Fid.example%2Fregistry%7Cquery-canary",
        Some("synthetic-west-credential"),
    );
    scoped_denial.headers_mut().insert(
        "x-request-id",
        axum::http::HeaderValue::from_static("00000000-0000-4000-8000-000000000003"),
    );
    let scoped_denial = router.oneshot(scoped_denial).await.unwrap();
    assert_eq!(scoped_denial.status(), StatusCode::FORBIDDEN);

    let events = audit.events.lock().unwrap().clone();
    assert_eq!(events.len(), 3);
    assert_eq!(
        events[0].event_type,
        SecurityEventType::ProtectedRouteDenied
    );
    assert_eq!(
        events[0].reason,
        SecurityEventReason::ConflictingCredentials
    );
    assert_eq!(events[0].route_family, SecurityRouteFamily::FhirPatient);
    assert_eq!(events[0].scope_shape, SecurityScopeShape::PathRecord);
    assert_eq!(events[0].request_id, "00000000-0000-4000-8000-000000000001");
    assert!(events[0].actor.is_none());
    assert_eq!(events[1].event_type, SecurityEventType::OidcCallbackDenied);
    assert_eq!(events[1].reason, SecurityEventReason::InvalidLoginFlow);
    assert_eq!(events[1].route_family, SecurityRouteFamily::OidcCallback);
    assert_eq!(events[1].scope_shape, SecurityScopeShape::None);
    assert_eq!(events[1].request_id, "00000000-0000-4000-8000-000000000002");
    assert!(events[1].actor.is_none());
    assert_eq!(
        events[2].event_type,
        SecurityEventType::ProtectedRouteDenied
    );
    assert_eq!(events[2].reason, SecurityEventReason::PermissionDenied);
    assert_eq!(events[2].route_family, SecurityRouteFamily::FhirPatient);
    assert_eq!(events[2].scope_shape, SecurityScopeShape::RequestBound);
    assert_eq!(events[2].request_id, "00000000-0000-4000-8000-000000000003");
    assert_eq!(
        events[2].actor.as_ref().map(|actor| actor.id.as_str()),
        Some("client-west")
    );

    let serialized = format!("{events:?}");
    for canary in [
        "record-canary",
        "query-code-canary",
        "query-token-canary",
        "bearer-token-canary-not-for-audit",
        "session-cookie-canary",
        "csrf-cookie-canary",
        "body-canary-not-for-audit",
        "oidc-code-canary",
        "oidc-state-canary",
        "flow-cookie-canary",
        "synthetic-west-credential",
    ] {
        assert!(!serialized.contains(canary), "audit leaked {canary}");
    }
}

fn request(uri: &str, credential: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder().uri(uri).method("GET");
    if let Some(credential) = credential {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {credential}"));
    }
    builder.body(Body::empty()).unwrap()
}

async fn body_json(response: axum::response::Response) -> Value {
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

fn synthetic_person() -> PersonView {
    PersonView {
        public_id: "litecr-synthetic-1".to_owned(),
        resource_version: 3,
        source_record_ids: vec!["source-synthetic-1".to_owned()],
        assertions: vec![SourceAssertionView {
            source_record_id: "source-synthetic-1".to_owned(),
            source_system_id: "source-east".to_owned(),
            source_version: Some("v1".to_owned()),
            receive_version: 1,
            field_path: "name.original".to_owned(),
            original_value: "ທົດສອບ ນາມສົມມຸດ".to_owned(),
            language_tag: Some("lo".to_owned()),
            script: None,
        }],
        aliases: Vec::new(),
        identifiers: vec![IdentifierView {
            authority: "https://id.example/registry".to_owned(),
            identifier_type: "{}".to_owned(),
            value: "synthetic-123".to_owned(),
            authoritative: false,
        }],
    }
}

#[tokio::test]
async fn composed_fhir_routes_require_middleware_auth_and_preserve_http_contract() {
    let (router, probe) = app();
    let unauthenticated = router
        .clone()
        .oneshot(request("/fhir/r4/metadata", None))
        .await
        .unwrap();
    assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(unauthenticated.headers()[header::CACHE_CONTROL], "no-store");
    assert_eq!(
        unauthenticated.headers()[header::CONTENT_TYPE],
        "application/fhir+json; charset=utf-8"
    );
    assert_eq!(unauthenticated.headers()["x-fhir-version"], "4.0.1");
    let unauthorized_outcome = body_json(unauthenticated).await;
    assert_eq!(unauthorized_outcome["resourceType"], "OperationOutcome");
    assert_eq!(unauthorized_outcome["issue"][0]["code"], "login");
    assert_eq!(unauthorized_outcome["issue"][0]["severity"], "error");

    let rejected_credential = router
        .clone()
        .oneshot(request("/fhir/r4/metadata", Some("wrong")))
        .await
        .unwrap();
    assert_eq!(rejected_credential.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        rejected_credential.headers()[header::CONTENT_TYPE],
        "application/fhir+json; charset=utf-8"
    );
    let rejected_outcome = body_json(rejected_credential).await;
    assert_eq!(rejected_outcome["resourceType"], "OperationOutcome");
    assert_eq!(rejected_outcome["issue"][0]["code"], "login");
    assert!(probe.searches.lock().unwrap().is_empty());

    let non_fhir_denial = router
        .clone()
        .oneshot(request("/api/v1/test/protected", None))
        .await
        .unwrap();
    assert_eq!(non_fhir_denial.status(), StatusCode::UNAUTHORIZED);
    assert!(
        non_fhir_denial.headers()[header::CONTENT_TYPE]
            .to_str()
            .unwrap()
            .starts_with("application/json")
    );
    let non_fhir_error = body_json(non_fhir_denial).await;
    assert_eq!(non_fhir_error["code"], "UNAUTHENTICATED");

    let metadata = router
        .clone()
        .oneshot(request(
            "/fhir/r4/metadata",
            Some("synthetic-east-credential"),
        ))
        .await
        .unwrap();
    assert_eq!(metadata.status(), StatusCode::OK);
    assert_eq!(
        metadata.headers()[header::CONTENT_TYPE],
        "application/fhir+json; charset=utf-8"
    );
    assert_eq!(metadata.headers()[header::CACHE_CONTROL], "no-store");
    assert_eq!(metadata.headers()["x-fhir-version"], "4.0.1");
    let metadata_json = body_json(metadata).await;
    assert_eq!(metadata_json["fhirVersion"], "4.0.1");
    let interactions = &metadata_json["rest"][0]["resource"][0]["interaction"];
    assert!(
        interactions
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["code"] == "create")
    );
}

#[tokio::test]
async fn unsupported_bundle_and_ihe_pix_operations_fail_without_registry_search_or_writes() {
    let (router, probe) = app();
    let bundle = serde_json::json!({
        "resourceType": "Bundle",
        "type": "transaction",
        "entry": []
    });
    let bundle_response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/fhir")
                .header(header::AUTHORIZATION, "Bearer synthetic-east-credential")
                .header(header::CONTENT_TYPE, "application/fhir+json")
                .body(Body::from(bundle.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(bundle_response.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        bundle_response.headers()[header::CONTENT_TYPE],
        "application/fhir+json; charset=utf-8"
    );
    let bundle_outcome = body_json(bundle_response).await;
    assert_eq!(bundle_outcome["resourceType"], "OperationOutcome");
    assert_eq!(bundle_outcome["issue"][0]["code"], "not-found");

    let ihe_pix_response = router
        .oneshot(request(
            "/fhir/Patient/$ihe-pix?sourceIdentifier=https%3A%2F%2Fid.example%7Csynthetic-123",
            Some("synthetic-east-credential"),
        ))
        .await
        .unwrap();
    assert_eq!(ihe_pix_response.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        ihe_pix_response.headers()[header::CONTENT_TYPE],
        "application/fhir+json; charset=utf-8"
    );
    assert_eq!(
        ihe_pix_response.headers()[header::CACHE_CONTROL],
        "no-store"
    );
    let outcome = body_json(ihe_pix_response).await;
    assert_eq!(outcome["resourceType"], "OperationOutcome");
    assert_eq!(outcome["issue"][0]["code"], "not-found");
    assert!(probe.person_lookups.lock().unwrap().is_empty());
    assert!(probe.searches.lock().unwrap().is_empty());
    assert!(probe.value_searches.lock().unwrap().is_empty());
    assert!(probe.ingestions.lock().unwrap().is_empty());
}

#[tokio::test]
async fn opencr_metadata_alias_matches_r4_metadata_and_keeps_fhir_auth_failures() {
    let (router, _) = app();
    let r4 = router
        .clone()
        .oneshot(request(
            "/fhir/r4/metadata",
            Some("synthetic-east-credential"),
        ))
        .await
        .unwrap();
    let alias = router
        .clone()
        .oneshot(request("/fhir/metadata", Some("synthetic-east-credential")))
        .await
        .unwrap();

    assert_eq!(r4.status(), StatusCode::OK);
    assert_eq!(alias.status(), StatusCode::OK);
    assert_eq!(
        alias.headers()[header::CONTENT_TYPE],
        "application/fhir+json; charset=utf-8"
    );
    assert_eq!(alias.headers()[header::CACHE_CONTROL], "no-store");
    assert_eq!(alias.headers()["x-fhir-version"], "4.0.1");
    assert_eq!(body_json(alias).await, body_json(r4).await);

    let unauthenticated = router
        .oneshot(request("/fhir/metadata", None))
        .await
        .unwrap();
    assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        unauthenticated.headers()[header::CONTENT_TYPE],
        "application/fhir+json; charset=utf-8"
    );
    assert_eq!(unauthenticated.headers()[header::CACHE_CONTROL], "no-store");
    let outcome = body_json(unauthenticated).await;
    assert_eq!(outcome["resourceType"], "OperationOutcome");
    assert_eq!(outcome["issue"][0]["code"], "login");
}

#[tokio::test]
async fn capability_statement_only_advertises_bound_scope_permissions() {
    let (router, _) = app();
    for (credential, expected_interactions, expected_search_param) in [
        ("synthetic-read-credential", vec!["read"], false),
        ("synthetic-search-credential", vec!["search-type"], true),
        ("synthetic-write-credential", vec!["create"], false),
        (
            "synthetic-disabled-create-credential",
            vec!["read", "search-type"],
            true,
        ),
    ] {
        let response = router
            .clone()
            .oneshot(request("/fhir/metadata", Some(credential)))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{credential}");
        let statement = body_json(response).await;
        let patient = &statement["rest"][0]["resource"][0];
        let actual_interactions = patient["interaction"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["code"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(actual_interactions, expected_interactions, "{credential}");
        assert_eq!(
            patient["searchParam"].as_array().unwrap().len(),
            if expected_search_param { 1 } else { 0 },
            "{credential}"
        );
        if expected_search_param {
            assert_eq!(patient["searchParam"][0]["name"], "identifier");
        }
    }

    // This client's server binding is org-east, while its only grants are in
    // org-west. A grant for another organization must not disclose any
    // operation on the bound organization.
    let wrong_org = router
        .oneshot(request(
            "/fhir/r4/metadata",
            Some("synthetic-west-credential"),
        ))
        .await
        .unwrap();
    assert_eq!(wrong_org.status(), StatusCode::OK);
    let statement = body_json(wrong_org).await;
    assert_eq!(statement["rest"][0]["resource"], serde_json::json!([]));
}

#[tokio::test]
async fn exact_identifier_search_uses_server_binding_and_registry_scope() {
    let (router, probe) = app();
    let response = router
        .oneshot(request(
            "/fhir/r4/Patient?identifier=https%3A%2F%2Fid.example%2Fregistry%7Csynthetic-123",
            Some("synthetic-east-credential"),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bundle = body_json(response).await;
    assert_eq!(bundle["resourceType"], "Bundle");
    assert_eq!(bundle["type"], "searchset");
    assert_eq!(bundle["total"], 0);
    assert_eq!(
        probe.searches.lock().unwrap().as_slice(),
        &[(
            REGISTRY.to_owned(),
            ORGANIZATION.to_owned(),
            "https://id.example/registry".to_owned(),
            "synthetic-123".to_owned(),
        )]
    );
}

#[tokio::test]
async fn opencr_patient_alias_accepts_value_only_search_within_bound_scope() {
    let (router, probe) = app();
    let response = router
        .oneshot(request(
            "/fhir/Patient?identifier=synthetic-123",
            Some("synthetic-search-credential"),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bundle = body_json(response).await;
    assert_eq!(bundle["resourceType"], "Bundle");
    assert_eq!(bundle["type"], "searchset");
    assert_eq!(bundle["total"], 0);
    assert_eq!(
        probe.value_searches.lock().unwrap().as_slice(),
        &[(
            REGISTRY.to_owned(),
            ORGANIZATION.to_owned(),
            "synthetic-123".to_owned()
        )]
    );
    assert!(probe.searches.lock().unwrap().is_empty());
}

#[tokio::test]
async fn csrf_middleware_denial_uses_fhir_operation_outcome() {
    let (router, _) = app();
    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/fhir/r4/Patient")
                .header(header::COOKIE, "litecr_session=synthetic-session")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        response.headers()[header::CONTENT_TYPE],
        "application/fhir+json; charset=utf-8"
    );
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    assert_eq!(response.headers()["x-fhir-version"], "4.0.1");
    let outcome = body_json(response).await;
    assert_eq!(outcome["resourceType"], "OperationOutcome");
    assert_eq!(outcome["issue"][0]["code"], "forbidden");
}

#[tokio::test]
async fn unauthenticated_opencr_patient_read_alias_returns_fhir_outcome() {
    let (router, _) = app();
    let response = router
        .oneshot(request("/fhir/Patient/litecr-synthetic-1", None))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        response.headers()[header::CONTENT_TYPE],
        "application/fhir+json; charset=utf-8"
    );
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    let outcome = body_json(response).await;
    assert_eq!(outcome["resourceType"], "OperationOutcome");
    assert_eq!(outcome["issue"][0]["code"], "login");
}

#[tokio::test]
async fn patient_read_uses_scoped_registry_lookup_and_projects_public_id() {
    let (router, _) = app();
    let response = router
        .oneshot(request(
            "/fhir/r4/Patient/litecr-synthetic-1",
            Some("synthetic-east-credential"),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()[header::CONTENT_TYPE],
        "application/fhir+json; charset=utf-8"
    );
    let patient = body_json(response).await;
    assert_eq!(patient["resourceType"], "Patient");
    assert_eq!(patient["id"], "litecr-synthetic-1");
    assert_eq!(
        patient["identifier"][0]["system"],
        "https://id.example/registry"
    );
    assert_eq!(patient["name"][0]["text"], "ທົດສອບ ນາມສົມມຸດ");
}

#[tokio::test]
async fn opencr_patient_read_alias_uses_the_same_scoped_registry_lookup() {
    let (router, _) = app();
    let response = router
        .oneshot(request(
            "/fhir/Patient/litecr-synthetic-1",
            Some("synthetic-east-credential"),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()[header::CONTENT_TYPE],
        "application/fhir+json; charset=utf-8"
    );
    let patient = body_json(response).await;
    assert_eq!(patient["resourceType"], "Patient");
    assert_eq!(patient["id"], "litecr-synthetic-1");
    assert_eq!(
        patient["identifier"][0]["system"],
        "https://id.example/registry"
    );
}

#[tokio::test]
async fn binding_scope_mismatch_is_denied_before_registry_access() {
    let (router, probe) = app();
    let response = router
        .oneshot(request(
            "/fhir/r4/Patient?identifier=https%3A%2F%2Fid.example%2Fregistry%7Csynthetic-123",
            Some("synthetic-west-credential"),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        response.headers()[header::CONTENT_TYPE],
        "application/fhir+json; charset=utf-8"
    );
    let outcome = body_json(response).await;
    assert_eq!(outcome["resourceType"], "OperationOutcome");
    assert!(probe.searches.lock().unwrap().is_empty());
}

#[tokio::test]
async fn create_requires_fhir_http_contract_and_uses_server_owned_source_scope() {
    let (router, probe) = app();
    let body = serde_json::json!({
        "resourceType": "Patient",
        "language": "lo",
        "identifier": [{
            "system": "https://id.example/registry",
            "value": "synthetic-123",
            "type": {"text": "local"}
        }],
        "name": [{"text": "ທົດສອບ ນາມສົມມຸດ"}],
        "birthDate": "1991-03"
    });
    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/fhir/r4/Patient")
                .header(header::AUTHORIZATION, "Bearer synthetic-east-credential")
                .header(header::CONTENT_TYPE, "application/fhir+json")
                .header("Idempotency-Key", "synthetic-http-create-1")
                .header("X-Source-Record-Key", "source-record-1")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let content_type = response.headers()[header::CONTENT_TYPE].clone();
    let location = response.headers()[header::LOCATION].clone();
    let response_json = body_json(response).await;
    assert_eq!(status, StatusCode::CREATED, "{response_json}");
    assert_eq!(content_type, "application/fhir+json; charset=utf-8");
    assert_eq!(location, "/fhir/r4/Patient/litecr-synthetic-1");
    let patient = response_json;
    assert_eq!(patient["resourceType"], "Patient");
    assert_eq!(patient["id"], "litecr-synthetic-1");
    let ingestions = probe.ingestions.lock().unwrap();
    assert_eq!(ingestions.len(), 1);
    let (idempotency_key, command) = &ingestions[0];
    assert_eq!(idempotency_key, "synthetic-http-create-1");
    assert_eq!(command.registry_id, REGISTRY);
    assert_eq!(command.organization_id, ORGANIZATION);
    assert_eq!(command.source_system_id, "source-east");
    assert_eq!(command.source_record_key, "source-record-1");
    assert_eq!(
        command.identifiers[0].authority,
        "https://id.example/registry"
    );
    assert_eq!(command.identifiers[0].value, "synthetic-123");
    assert!(
        command
            .assertions
            .iter()
            .any(|assertion| assertion.original_value == "1991-03")
    );
}

#[tokio::test]
async fn replayed_fhir_create_returns_ok_with_stable_location_without_duplicate_ingestion() {
    let (router, probe) = app();
    let body = serde_json::json!({
        "resourceType": "Patient",
        "identifier": [{
            "system": "https://id.example/registry",
            "value": "synthetic-replay-123",
            "type": {"text": "local"}
        }],
        "name": [{"text": "Synthetic replay patient"}]
    });
    let send = |body: &Value| {
        Request::builder()
            .method("POST")
            .uri("/fhir/Patient")
            .header(header::AUTHORIZATION, "Bearer synthetic-east-credential")
            .header(header::CONTENT_TYPE, "application/fhir+json")
            .header("Idempotency-Key", "synthetic-fhir-replay-1")
            .header("X-Source-Record-Key", "synthetic-fhir-replay-source")
            .body(Body::from(body.to_string()))
            .unwrap()
    };

    let first = router.clone().oneshot(send(&body)).await.unwrap();
    assert_eq!(first.status(), StatusCode::CREATED);
    let first_location = first.headers()[header::LOCATION].clone();
    let first_body = body_json(first).await;

    let replay = router.oneshot(send(&body)).await.unwrap();
    assert_eq!(replay.status(), StatusCode::OK);
    assert_eq!(replay.headers()[header::LOCATION], first_location);
    assert_eq!(
        replay.headers()[header::CONTENT_TYPE],
        "application/fhir+json; charset=utf-8"
    );
    assert_eq!(body_json(replay).await, first_body);
    assert_eq!(probe.ingestions.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn changed_fhir_create_reusing_idempotency_key_returns_conflict_without_second_ingestion() {
    let (router, probe) = app();
    let mut first_body = serde_json::json!({
        "resourceType": "Patient",
        "identifier": [{
            "system": "https://id.example/registry",
            "value": "synthetic-conflict-123",
            "type": {"text": "local"}
        }],
        "name": [{"text": "Synthetic conflict patient"}]
    });
    let send = |body: &Value| {
        Request::builder()
            .method("POST")
            .uri("/fhir/Patient")
            .header(header::AUTHORIZATION, "Bearer synthetic-east-credential")
            .header(header::CONTENT_TYPE, "application/fhir+json")
            .header("Idempotency-Key", "synthetic-fhir-conflict-1")
            .header("X-Source-Record-Key", "synthetic-fhir-conflict-source")
            .body(Body::from(body.to_string()))
            .unwrap()
    };

    let first = router.clone().oneshot(send(&first_body)).await.unwrap();
    assert_eq!(first.status(), StatusCode::CREATED);
    assert_eq!(probe.ingestions.lock().unwrap().len(), 1);

    first_body["birthDate"] = serde_json::json!("1992-04");
    let conflict = router.oneshot(send(&first_body)).await.unwrap();
    assert_eq!(conflict.status(), StatusCode::CONFLICT);
    assert_eq!(
        conflict.headers()[header::CONTENT_TYPE],
        "application/fhir+json; charset=utf-8"
    );
    let outcome = body_json(conflict).await;
    assert_eq!(outcome["resourceType"], "OperationOutcome");
    assert_eq!(outcome["issue"][0]["severity"], "error");
    assert_eq!(outcome["issue"][0]["code"], "conflict");
    assert_eq!(
        outcome["issue"][0]["diagnostics"],
        "The idempotency key was reused with a different command."
    );
    assert_eq!(probe.ingestions.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn opencr_create_accepts_application_json_and_rejects_non_fhir_media_without_writes() {
    let body = serde_json::json!({
        "resourceType": "Patient",
        "identifier": [{
            "system": "https://id.example/registry",
            "value": "synthetic-media-type-123",
            "type": {"text": "local"}
        }],
        "name": [{"text": "Synthetic media type patient"}]
    });

    let (router, accepted_probe) = app();
    let accepted = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/fhir/Patient")
                .header(header::AUTHORIZATION, "Bearer synthetic-east-credential")
                .header(header::CONTENT_TYPE, "application/json")
                .header("Idempotency-Key", "synthetic-json-media-create")
                .header("X-Source-Record-Key", "synthetic-json-media-source")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(accepted.status(), StatusCode::CREATED);
    assert_eq!(
        accepted.headers()[header::CONTENT_TYPE],
        "application/fhir+json; charset=utf-8"
    );
    assert_eq!(body_json(accepted).await["resourceType"], "Patient");
    {
        let accepted_writes = accepted_probe.ingestions.lock().unwrap();
        assert_eq!(accepted_writes.len(), 1);
        assert_eq!(accepted_writes[0].1.registry_id, REGISTRY);
        assert_eq!(accepted_writes[0].1.organization_id, ORGANIZATION);
    }

    let (router, rejected_probe) = app();
    let rejected = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/fhir/Patient")
                .header(header::AUTHORIZATION, "Bearer synthetic-east-credential")
                .header(header::CONTENT_TYPE, "text/plain")
                .header("Idempotency-Key", "synthetic-plain-media-create")
                .header("X-Source-Record-Key", "synthetic-plain-media-source")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(rejected.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        rejected.headers()[header::CONTENT_TYPE],
        "application/fhir+json; charset=utf-8"
    );
    let outcome = body_json(rejected).await;
    assert_eq!(outcome["resourceType"], "OperationOutcome");
    assert_eq!(outcome["issue"][0]["code"], "invalid");
    assert!(rejected_probe.ingestions.lock().unwrap().is_empty());
}

#[tokio::test]
async fn opencr_published_identifier_without_type_is_rejected_without_ingestion() {
    // The public OpenCR demo Patient includes Identifier.system and value but
    // omits optional Identifier.type. liteCR's identity namespace currently
    // requires a type, so this documented input shape must fail closed rather
    // than inventing a placeholder or writing a partial source record.
    let body = serde_json::json!({
        "resourceType": "Patient",
        "identifier": [{
            "system": "https://id.example/registry",
            "value": "synthetic-opencr-sample-123"
        }]
    });
    let (router, probe) = app();
    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/fhir/Patient")
                .header(header::AUTHORIZATION, "Bearer synthetic-east-credential")
                .header(header::CONTENT_TYPE, "application/json")
                .header("Idempotency-Key", "synthetic-opencr-missing-type")
                .header("X-Source-Record-Key", "synthetic-opencr-missing-type")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        response.headers()[header::CONTENT_TYPE],
        "application/fhir+json; charset=utf-8"
    );
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    let outcome = body_json(response).await;
    assert_eq!(outcome["resourceType"], "OperationOutcome");
    assert_eq!(outcome["issue"][0]["code"], "invalid");
    assert!(probe.ingestions.lock().unwrap().is_empty());
}

#[tokio::test]
async fn opencr_patient_alias_uses_the_same_scoped_create_contract() {
    let (router, probe) = app();
    let body = serde_json::json!({
        "resourceType": "Patient",
        "language": "lo",
        "identifier": [{
            "system": "https://id.example/registry",
            "value": "synthetic-123",
            "type": {"text": "local"}
        }],
        "name": [{"text": "ທົດສອບ ນາມສົມມຸດ"}],
        "birthDate": "1991-03"
    });
    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/fhir/Patient")
                .header(header::AUTHORIZATION, "Bearer synthetic-east-credential")
                .header(header::CONTENT_TYPE, "application/fhir+json")
                .header("Idempotency-Key", "synthetic-http-create-alias-1")
                .header("X-Source-Record-Key", "source-record-alias-1")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let content_type = response.headers()[header::CONTENT_TYPE].clone();
    let location = response.headers()[header::LOCATION].clone();
    let response_json = body_json(response).await;

    assert_eq!(status, StatusCode::CREATED, "{response_json}");
    assert_eq!(content_type, "application/fhir+json; charset=utf-8");
    assert_eq!(location, "/fhir/r4/Patient/litecr-synthetic-1");
    assert_eq!(response_json["resourceType"], "Patient");
    assert_eq!(response_json["id"], "litecr-synthetic-1");

    let ingestions = probe.ingestions.lock().unwrap();
    assert_eq!(ingestions.len(), 1);
    let (idempotency_key, command) = &ingestions[0];
    assert_eq!(idempotency_key, "synthetic-http-create-alias-1");
    assert_eq!(command.registry_id, REGISTRY);
    assert_eq!(command.organization_id, ORGANIZATION);
    assert_eq!(command.source_system_id, "source-east");
    assert_eq!(command.source_record_key, "source-record-alias-1");
    assert_eq!(
        command.identifiers[0].authority,
        "https://id.example/registry"
    );
    assert_eq!(command.identifiers[0].value, "synthetic-123");
}
