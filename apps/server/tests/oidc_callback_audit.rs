use async_trait::async_trait;
use axum::{
    body::Body,
    http::{Request, StatusCode, header},
};
use litecr_api::access::{AccessApiConfig, AccessEnvironment};
use litecr_application::access::{
    AccessAuthenticator, AccessDirectory, AccessDirectoryError, AccessIdentity,
    AccessSecurityAudit, AccessService, AccessServiceConfig, AccessSession,
    AccessSessionRepository, ActorRef, OidcAuthorization, OidcFlow, OidcFlowSecrets, OidcFlowStore,
    OidcIdentityClaims, OidcProvider, OidcProviderError, SessionCredentials, SessionError,
};
use litecr_infrastructure::{access::PostgresAccessSecurityAudit, connect};
use sqlx::PgPool;
use std::{
    env,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};
use tower::ServiceExt;

const TEST_DATABASE_ENV: &str = "LITECR_TEST_DATABASE_URL";

struct Unused;

#[async_trait]
impl OidcProvider for Unused {
    async fn begin_authorization(&self) -> Result<OidcAuthorization, OidcProviderError> {
        Err(OidcProviderError::Unavailable)
    }

    async fn exchange_code(
        &self,
        _: &str,
        _: &OidcFlow,
    ) -> Result<OidcIdentityClaims, OidcProviderError> {
        Err(OidcProviderError::Unavailable)
    }
}

#[async_trait]
impl OidcFlowStore for Unused {
    async fn create(&self, _: &OidcFlowSecrets, _: i64) -> Result<(), SessionError> {
        Err(SessionError::Unavailable)
    }

    async fn consume(&self, _: &str, _: &str) -> Result<Option<OidcFlow>, SessionError> {
        Ok(None)
    }
}

#[async_trait]
impl AccessSessionRepository for Unused {
    async fn create(
        &self,
        _: &ActorRef,
        _: Option<&str>,
        _: i64,
        _: &str,
    ) -> Result<SessionCredentials, SessionError> {
        Err(SessionError::Unavailable)
    }
    async fn resolve(&self, _: &str) -> Result<Option<AccessSession>, SessionError> {
        Err(SessionError::Unavailable)
    }
    async fn verify_csrf(&self, _: &str, _: &str) -> Result<bool, SessionError> {
        Err(SessionError::Unavailable)
    }
    async fn revoke(&self, _: &str, _: &str) -> Result<(), SessionError> {
        Err(SessionError::Unavailable)
    }
    async fn revoke_actor_sessions(&self, _: &ActorRef, _: &str) -> Result<u64, SessionError> {
        Err(SessionError::Unavailable)
    }
}

#[async_trait]
impl AccessDirectory for Unused {
    async fn load_identity(
        &self,
        _: litecr_application::access::ActorKind,
        _: Option<&str>,
        _: &str,
    ) -> Result<Option<AccessIdentity>, AccessDirectoryError> {
        Ok(None)
    }
}

#[async_trait]
impl AccessAuthenticator for Unused {
    async fn authenticate_cookie(
        &self,
        _: &str,
        _: Option<&str>,
        _: bool,
        _: &str,
    ) -> Result<AccessIdentity, SessionError> {
        Err(SessionError::Invalid)
    }
    async fn authenticate_integration_credential(
        &self,
        _: &str,
        _: &str,
    ) -> Result<AccessIdentity, SessionError> {
        Err(SessionError::Invalid)
    }
}

fn test_id(suffix: &str) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let discriminator = if suffix == "persisted" { 1_u64 } else { 2_u64 };
    let tail = (now as u64 ^ std::process::id() as u64 ^ discriminator) & 0x0000_ffff_ffff_ffff;
    format!("00000000-0000-4000-8000-{tail:012x}")
}

fn callback_router(pool: PgPool) -> axum::Router {
    let audit: Arc<dyn AccessSecurityAudit> = Arc::new(PostgresAccessSecurityAudit::new(pool));
    let service = Arc::new(
        AccessService::new(
            Arc::new(Unused),
            Arc::new(Unused),
            Arc::new(Unused),
            Arc::new(Unused),
            Arc::new(Unused),
            audit,
            AccessServiceConfig {
                login_flow_lifetime_seconds: 300,
                session_lifetime_seconds: 3600,
            },
        )
        .expect("valid access service config"),
    );
    litecr_api::access::router(
        service,
        AccessApiConfig {
            expected_origin: "http://localhost".into(),
            secure_cookies: false,
            environment: AccessEnvironment::Development,
            session_lifetime_seconds: 3600,
            flow_lifetime_seconds: 300,
        },
    )
}

async fn send_callback(router: axum::Router, request_id: &str) -> axum::response::Response {
    router
        .oneshot(
            Request::builder()
                .uri("/api/v1/access/oidc/callback?state=callback-state-canary&code=callback-code-canary")
                .header("x-request-id", request_id)
                .header(header::COOKIE, "litecr_oidc_flow=callback-flow-canary")
                .body(Body::empty())
                .expect("valid request"),
        )
        .await
        .expect("callback response")
}

#[tokio::test]
async fn oidc_callback_records_denial_through_postgres_sink_and_fails_closed() {
    let Ok(database_url) = env::var(TEST_DATABASE_ENV) else {
        eprintln!(
            "skipping: set {TEST_DATABASE_ENV} to an isolated synthetic schema-23 PostgreSQL database"
        );
        return;
    };
    let pool = connect(&database_url, 2)
        .await
        .expect("connect to test database");
    let success_id = test_id("persisted");
    let failure_id = test_id("failed");
    let response = send_callback(callback_router(pool.clone()), &success_id).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert!(
        response
            .headers()
            .get(header::SET_COOKIE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.contains("Max-Age=0"))
    );
    let response_body = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .expect("read generic response body");
    let response_body = String::from_utf8_lossy(&response_body);
    assert!(!response_body.contains("callback-state-canary"));
    assert!(!response_body.contains("callback-code-canary"));
    assert!(!response_body.contains("callback-flow-canary"));

    let event = sqlx::query_as::<_, (String, String, String, String, Option<String>, Option<String>)>(
        "SELECT event_type, reason_code, route_family, scope_shape, actor_kind, actor_identity_key::text FROM litecr.access_security_audit WHERE request_id=$1",
    ).bind(&success_id).fetch_one(&pool).await.expect("callback denial is durable");
    assert_eq!(
        event,
        (
            "oidc_callback_denied".into(),
            "invalid_login_flow".into(),
            "oidc_callback".into(),
            "none".into(),
            None,
            None,
        )
    );

    pool.close().await;
    let response = send_callback(callback_router(pool), &failure_id).await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(
        response
            .headers()
            .get(header::SET_COOKIE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.contains("Max-Age=0"))
    );
    let response_body = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .expect("read generic failure response body");
    let response_body = String::from_utf8_lossy(&response_body);
    assert!(!response_body.contains("callback-state-canary"));
    assert!(!response_body.contains("callback-code-canary"));
    assert!(!response_body.contains("callback-flow-canary"));
    let cleanup_pool = connect(&database_url, 1)
        .await
        .expect("reconnect to clean synthetic audit row");
    let mut cleanup = cleanup_pool.begin().await.expect("begin synthetic cleanup");
    sqlx::query(
        "ALTER TABLE litecr.access_security_audit DISABLE TRIGGER access_security_audit_immutable",
    )
    .execute(&mut *cleanup)
    .await
    .expect("temporarily permit isolated fixture cleanup");
    sqlx::query("DELETE FROM litecr.access_security_audit WHERE request_id = ANY($1)")
        .bind(vec![success_id, failure_id])
        .execute(&mut *cleanup)
        .await
        .expect("remove exact synthetic audit request IDs");
    sqlx::query(
        "ALTER TABLE litecr.access_security_audit ENABLE TRIGGER access_security_audit_immutable",
    )
    .execute(&mut *cleanup)
    .await
    .expect("restore append-only protection");
    cleanup.commit().await.expect("commit isolated cleanup");
    cleanup_pool.close().await;
}
