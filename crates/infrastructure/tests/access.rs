use std::sync::Arc;

use litecr_application::access::{
    AccessAuthenticator, AccessDirectory, AccessSecurityAudit, AccessSessionRepository, ActorKind,
    ActorRef, McpAccessService, McpCredentialProfile, McpInvocationBoundary, McpInvocationDenial,
    McpToolId, OidcFlowSecrets, OidcFlowStore, Permission, Role, SecurityAuditEvent,
    SecurityEventReason, SecurityEventType, SecurityRouteFamily, SecurityScopeShape, SessionError,
};
use litecr_infrastructure::{
    access::{
        PostgresAccessAuthenticator, PostgresAccessDirectory, PostgresAccessSecurityAudit,
        PostgresAccessSessionRepository, PostgresMcpInvocationAudit, PostgresOidcFlowStore,
    },
    connect, migrate,
};
use sha2::{Digest, Sha256};
use sqlx::Row;

fn suffix() -> String {
    format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    )
}

fn audit_reference(value: &str) -> String {
    let mut result = String::with_capacity(64);
    for byte in Sha256::digest(value.as_bytes()) {
        use std::fmt::Write as _;
        write!(result, "{byte:02x}").expect("writing into a String cannot fail");
    }
    result
}

#[tokio::test]
async fn postgres_access_flow_session_csrf_scope_and_revocation() {
    let database_url = std::env::var("LITECR_TEST_DATABASE_URL")
        .expect("set LITECR_TEST_DATABASE_URL to the dedicated synthetic-only litecr_test DB");
    let key = suffix();
    let result = tokio::spawn(run_access_scenario(database_url.clone(), key.clone())).await;
    cleanup_test_rows(&database_url, &key).await;
    assert!(result.is_ok(), "access scenario task failed: {result:?}");
}

#[tokio::test]
async fn postgres_security_audit_persists_bounded_events_and_rejects_mutation() {
    let database_url = std::env::var("LITECR_TEST_DATABASE_URL")
        .expect("set LITECR_TEST_DATABASE_URL to the dedicated synthetic-only litecr_test DB");
    let pool = connect(&database_url, 2)
        .await
        .expect("dedicated audit test DB reachable");
    migrate(&pool)
        .await
        .expect("schema 22 internal audit actor migration applies");
    let sink = PostgresAccessSecurityAudit::new(pool.clone());
    let request_id = format!("security-audit-test-{}", suffix());
    let external_subject = format!("synthetic-subject-{}", suffix());
    let issuer = format!("https://synthetic-idp-{}.example.test", suffix());
    let internal_identity_key: i64 = sqlx::query_scalar(
        "INSERT INTO litecr.access_identity (actor_kind, issuer_url, external_subject) \
         VALUES ('human', $1, $2) RETURNING identity_key",
    )
    .bind(&issuer)
    .bind(&external_subject)
    .fetch_one(&pool)
    .await
    .expect("synthetic access identity provisioned");
    sink.append(&SecurityAuditEvent {
        event_type: SecurityEventType::ProtectedRouteDenied,
        reason: SecurityEventReason::PermissionDenied,
        route_family: SecurityRouteFamily::Review,
        scope_shape: SecurityScopeShape::PathRecord,
        actor: Some(ActorRef {
            kind: ActorKind::Human,
            id: external_subject.clone(),
            issuer_url: Some(issuer.clone()),
        }),
        request_id: request_id.clone(),
    })
    .await
    .expect("event appends");

    let row = sqlx::query(
        "SELECT event_type, reason_code, route_family, scope_shape, actor_kind, actor_identity_key, request_id, \
         to_jsonb(access_security_audit)::text AS row_json \
         FROM litecr.access_security_audit WHERE request_id = $1",
    )
    .bind(&request_id)
    .fetch_one(&pool)
    .await
    .expect("appended event persists");
    assert_eq!(row.get::<String, _>("event_type"), "protected_route_denied");
    assert_eq!(row.get::<String, _>("reason_code"), "permission_denied");
    assert_eq!(row.get::<String, _>("route_family"), "review");
    assert_eq!(row.get::<String, _>("scope_shape"), "path_record");
    assert_eq!(
        row.get::<Option<String>, _>("actor_kind").as_deref(),
        Some("human")
    );
    assert_eq!(
        row.get::<Option<i64>, _>("actor_identity_key"),
        Some(internal_identity_key)
    );
    assert_eq!(row.get::<String, _>("request_id"), request_id);

    let columns: Vec<String> = sqlx::query_scalar(
        "SELECT column_name FROM information_schema.columns WHERE table_schema = 'litecr' \
         AND table_name = 'access_security_audit' ORDER BY ordinal_position",
    )
    .fetch_all(&pool)
    .await
    .expect("audit schema inspected");
    assert_eq!(
        columns,
        [
            "event_id",
            "occurred_at",
            "event_type",
            "reason_code",
            "route_family",
            "scope_shape",
            "actor_kind",
            "request_id",
            "actor_identity_key",
        ]
        .map(str::to_owned)
    );
    let serialized = row.get::<String, _>("row_json");
    for forbidden_column in [
        "actor_id",
        "external_subject",
        "issuer_url",
        "request_uri",
        "query_string",
        "request_body",
        "cookie_value",
        "token_value",
        "authorization_header",
        "oidc_code",
        "oidc_state",
    ] {
        assert!(!columns.iter().any(|column| column == forbidden_column));
    }
    for canary in [
        "token-canary",
        "code-canary",
        "cookie-canary",
        "query-canary",
        "body-canary",
        external_subject.as_str(),
        issuer.as_str(),
    ] {
        assert!(!serialized.contains(canary));
    }

    assert!(sqlx::query(
        "UPDATE litecr.access_security_audit SET reason_code = 'invalid_callback' WHERE request_id = $1",
    )
    .bind(&request_id)
    .execute(&pool)
    .await
    .is_err());
    assert!(
        sqlx::query("DELETE FROM litecr.access_security_audit WHERE request_id = $1")
            .bind(&request_id)
            .execute(&pool)
            .await
            .is_err()
    );
    let retained: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM litecr.access_security_audit WHERE request_id = $1 AND event_type = 'protected_route_denied'",
    )
    .bind(&request_id)
    .fetch_one(&pool)
    .await
    .expect("append-only event remains queryable");
    assert_eq!(retained, 1);

    let login_failure_request_id = format!("oidc-denied-{}", suffix());
    sink.append(&SecurityAuditEvent {
        event_type: SecurityEventType::OidcCallbackDenied,
        reason: SecurityEventReason::IdentityProviderRejected,
        route_family: SecurityRouteFamily::OidcCallback,
        scope_shape: SecurityScopeShape::None,
        actor: None,
        request_id: login_failure_request_id.clone(),
    })
    .await
    .expect("failed OIDC login denial appends durably");
    let login_failure: (String, String, String, Option<i64>) = sqlx::query_as(
        "SELECT event_type, reason_code, route_family, actor_identity_key \
         FROM litecr.access_security_audit WHERE request_id = $1",
    )
    .bind(&login_failure_request_id)
    .fetch_one(&pool)
    .await
    .expect("failed OIDC login denial persists");
    assert_eq!(
        login_failure,
        (
            "oidc_callback_denied".to_owned(),
            "identity_provider_rejected".to_owned(),
            "oidc_callback".to_owned(),
            None,
        )
    );

    let unknown_request_id = format!("security-audit-unknown-{}", suffix());
    sink.append(&SecurityAuditEvent {
        event_type: SecurityEventType::SessionAuthenticationDenied,
        reason: SecurityEventReason::MissingCredential,
        route_family: SecurityRouteFamily::Session,
        scope_shape: SecurityScopeShape::None,
        actor: None,
        request_id: unknown_request_id.clone(),
    })
    .await
    .expect("unknown actor event appends");
    let unknown_actor: (Option<String>, Option<i64>) = sqlx::query_as(
        "SELECT actor_kind, actor_identity_key FROM litecr.access_security_audit \
         WHERE request_id = $1",
    )
    .bind(unknown_request_id)
    .fetch_one(&pool)
    .await
    .expect("unknown actor event persists");
    assert_eq!(unknown_actor, (None, None));
    pool.close().await;
}

#[tokio::test]
async fn postgres_mcp_credentials_are_separate_scoped_revocable_and_audit_is_redacted() {
    let database_url = std::env::var("LITECR_TEST_DATABASE_URL")
        .expect("set LITECR_TEST_DATABASE_URL to the dedicated synthetic-only litecr_test DB");
    let key = suffix();
    let result = tokio::spawn(run_mcp_credential_audit_scenario(
        database_url.clone(),
        key.clone(),
    ))
    .await;
    cleanup_mcp_test_rows(&database_url, &key).await;
    assert!(
        result.is_ok(),
        "MCP credential/audit scenario failed: {result:?}"
    );
}

async fn cleanup_mcp_test_rows(database_url: &str, key: &str) {
    let pool = connect(database_url, 2)
        .await
        .expect("dedicated access DB reachable for MCP test cleanup");
    let subject = format!("mcp-client-{key}");
    let issuer = "https://synthetic-mcp-idp.example.test";
    // Invocation audit is intentionally append-only. Keep its synthetic rows
    // (and the referenced identity) in the dedicated test database.
    sqlx::query(
        "DELETE FROM litecr.access_mcp_stdio_credential_tool WHERE credential_id IN \
         (SELECT credential_id FROM litecr.access_mcp_stdio_credential WHERE issuer_url = $1 AND external_subject = $2)",
    )
    .bind(issuer)
    .bind(&subject)
    .execute(&pool)
    .await
    .expect("exact-suffix MCP credential tools are cleaned");
    sqlx::query(
        "DELETE FROM litecr.access_mcp_stdio_credential WHERE issuer_url = $1 AND external_subject = $2",
    )
    .bind(issuer)
    .bind(&subject)
    .execute(&pool)
    .await
    .expect("exact-suffix MCP credentials are cleaned");
    sqlx::query(
        "DELETE FROM litecr.access_audit WHERE actor_issuer_url = $1 AND actor_id = $2 AND action LIKE 'mcp.credential.%'",
    )
    .bind(issuer)
    .bind(&subject)
    .execute(&pool)
    .await
    .expect("exact-suffix credential lifecycle audit is cleaned");
    sqlx::query(
        "DELETE FROM litecr.access_grant WHERE actor_kind = 'integration_client' AND issuer_url = $1 AND external_subject = $2",
    )
    .bind(issuer)
    .bind(&subject)
    .execute(&pool)
    .await
    .expect("exact-suffix MCP grants are cleaned");
    let remaining: i64 = sqlx::query_scalar(
        "SELECT (SELECT COUNT(*) FROM litecr.access_mcp_stdio_credential WHERE external_subject = $1) + \
         (SELECT COUNT(*) FROM litecr.access_mcp_stdio_credential_tool t JOIN litecr.access_mcp_stdio_credential c USING (credential_id) WHERE c.external_subject = $1) + \
         (SELECT COUNT(*) FROM litecr.access_grant WHERE external_subject = $1)",
    )
    .bind(subject)
    .fetch_one(&pool)
    .await
    .expect("exact-suffix cleanup postcondition queried");
    assert_eq!(remaining, 0);
    pool.close().await;
}

async fn run_mcp_credential_audit_scenario(database_url: String, key: String) {
    let pool = connect(&database_url, 4)
        .await
        .expect("dedicated MCP integration DB reachable");
    migrate(&pool)
        .await
        .expect("latest schema migration applies");
    let issuer = "https://synthetic-mcp-idp.example.test";
    let subject = format!("mcp-client-{key}");
    sqlx::query(
        "INSERT INTO litecr.access_identity (actor_kind, issuer_url, external_subject) VALUES ('integration_client', $1, $2)",
    )
    .bind(issuer)
    .bind(&subject)
    .execute(&pool)
    .await
    .expect("synthetic MCP IntegrationClient is provisioned");
    sqlx::query(
        "INSERT INTO litecr.access_grant (actor_kind, issuer_url, external_subject, registry_id, organization_id, role_name) \
         VALUES ('integration_client', $1, $2, 'registry-mcp-synthetic', 'org-mcp-synthetic', 'integration_client')",
    )
    .bind(issuer)
    .bind(&subject)
    .execute(&pool)
    .await
    .expect("synthetic IntegrationClient role is granted");
    for permission in ["person:read", "person:search"] {
        sqlx::query(
            "INSERT INTO litecr.access_grant (actor_kind, issuer_url, external_subject, registry_id, organization_id, record_ids, permission_name) \
             VALUES ('integration_client', $1, $2, 'registry-mcp-synthetic', 'org-mcp-synthetic', NULL, $3)",
        )
        .bind(issuer)
        .bind(&subject)
        .bind(permission)
        .execute(&pool)
        .await
        .expect("synthetic IntegrationClient tool permission is granted");
    }

    let directory = PostgresAccessDirectory::new(pool.clone());
    let sessions = Arc::new(PostgresAccessSessionRepository::new(
        pool.clone(),
        directory.clone(),
    ));
    let access = McpAccessService::new(Arc::new(directory.clone()), sessions.clone());
    let profile = McpCredentialProfile::new(
        format!("synthetic-client-{key}"),
        "synthetic-purpose-test".to_owned(),
        [
            McpToolId::RegistryGetRecordSummary,
            McpToolId::RegistrySearchRecords,
        ],
        600,
    )
    .expect("test-only profile uses explicit tools and bounded TTL");
    let secret = access
        .issue(
            Some(issuer),
            &subject,
            &profile,
            &format!("mcp-{key}-issued"),
        )
        .await
        .expect("provisioned actor with both grants can receive a test credential");
    assert!(secret.len() >= 43, "opaque secret has sufficient entropy");
    let token_hash = Sha256::digest(secret.as_bytes()).to_vec();
    let stored: (Vec<u8>, String, String, String, bool) = sqlx::query_as(
        "SELECT token_hash, audience, client_id, purpose_code, expires_at <= now() + interval '15 minutes' \
         FROM litecr.access_mcp_stdio_credential WHERE token_hash = $1",
    )
    .bind(&token_hash)
    .fetch_one(&pool)
    .await
    .expect("dedicated MCP credential row exists");
    assert_eq!(stored.0, token_hash);
    assert_eq!(stored.1, "urn:litecr:mcp:stdio");
    assert_eq!(stored.2, profile.client_id);
    assert_eq!(stored.3, profile.purpose_code);
    assert!(stored.4);

    let generic_auth = PostgresAccessAuthenticator::new((*sessions).clone());
    assert!(matches!(
        generic_auth
            .authenticate_integration_credential(&secret, &format!("mcp-{key}-generic"))
            .await,
        Err(litecr_application::access::SessionError::Invalid)
    ));

    let first = access
        .authenticate(&secret, &format!("mcp-{key}-first"))
        .await
        .expect("MCP audience token authenticates only through MCP path");
    assert!(first.allows_tool(McpToolId::RegistryGetRecordSummary));
    assert!(first.allows_tool(McpToolId::RegistrySearchRecords));
    let summary_invocation = first
        .invocation_for_tool(
            McpToolId::RegistryGetRecordSummary,
            "registry-mcp-synthetic",
            "org-mcp-synthetic",
        )
        .expect("record-summary context is available for its allowed tool and scope");
    let summary_context = summary_invocation.access_context();
    assert_eq!(summary_context.purpose(), Some("synthetic-purpose-test"));
    assert!(summary_context.grants().iter().all(|grant| {
        grant.permissions == std::collections::BTreeSet::from([Permission::PersonRead])
            && grant.roles.is_empty()
    }));
    let search_invocation = first
        .invocation_for_tool(
            McpToolId::RegistrySearchRecords,
            "registry-mcp-synthetic",
            "org-mcp-synthetic",
        )
        .expect("search context is available for its allowed tool and scope");
    let search_context = search_invocation.access_context();
    assert!(search_context.grants().iter().all(|grant| {
        grant.permissions == std::collections::BTreeSet::from([Permission::PersonSearch])
            && grant.roles.is_empty()
    }));

    sqlx::query(
        "DELETE FROM litecr.access_grant WHERE actor_kind = 'integration_client' AND issuer_url = $1 AND external_subject = $2 AND permission_name = 'person:search'",
    )
    .bind(issuer)
    .bind(&subject)
    .execute(&pool)
    .await
    .expect("search grant revocation applies");
    let refreshed = access
        .authenticate(&secret, &format!("mcp-{key}-refreshed"))
        .await
        .expect("credential reloads current IntegrationClient grants");
    assert!(refreshed.allows_tool(McpToolId::RegistryGetRecordSummary));
    assert!(!refreshed.allows_tool(McpToolId::RegistrySearchRecords));

    let audit = PostgresMcpInvocationAudit::new(pool.clone());
    let boundary = McpInvocationBoundary::new(Arc::new(audit));
    let audit_request = format!("mcp-{key}-first");
    let success_audit_reference = audit_reference(&audit_request);
    let success_invocation = first
        .invocation_for_tool(
            McpToolId::RegistryGetRecordSummary,
            "registry-mcp-synthetic",
            "org-mcp-synthetic",
        )
        .expect("success attribution binds authenticated client/tool/current tenant grant");
    boundary
        .before_reply(success_invocation, Ok(((), 1)))
        .await
        .expect("redacted success audit appends durably");
    let denied_request = format!("mcp-{key}-audit-denied");
    boundary
        .record_unattributed_denial(
            denied_request.clone(),
            McpToolId::RegistrySearchRecords,
            McpInvocationDenial::InvalidCredential,
        )
        .await
        .expect("anonymous denied tool attempt is durably recorded");
    let row_json: String = sqlx::query_scalar(
        "SELECT to_jsonb(mcp_invocation_audit)::text FROM litecr.mcp_invocation_audit WHERE request_id = $1",
    )
    .bind(success_audit_reference)
    .fetch_one(&pool)
    .await
    .expect("MCP audit row is queryable");
    for forbidden in [
        subject.as_str(),
        "patient-canary",
        "argument-canary",
        "result-canary",
    ] {
        assert!(!row_json.contains(forbidden), "audit excludes {forbidden}");
    }
    assert!(
        sqlx::query(
            "UPDATE litecr.mcp_invocation_audit SET outcome = 'denied' WHERE request_id = $1",
        )
        .bind(audit_reference(&denied_request))
        .execute(&pool)
        .await
        .is_err()
    );
    assert!(
        sqlx::query("DELETE FROM litecr.mcp_invocation_audit WHERE request_id = $1")
            .bind(audit_reference(&denied_request))
            .execute(&pool)
            .await
            .is_err()
    );

    assert!(
        access
            .revoke(&secret, &format!("mcp-{key}-revoke"))
            .await
            .expect("MCP credential revocation commits")
    );
    assert!(matches!(
        access
            .authenticate(&secret, &format!("mcp-{key}-after-revoke"))
            .await,
        Err(litecr_application::access::SessionError::Invalid)
    ));
    pool.close().await;
}

async fn cleanup_test_rows(database_url: &str, key: &str) {
    let pool = connect(database_url, 2)
        .await
        .expect("dedicated access DB reachable for exact-suffix cleanup");
    let human_subject = format!("human-{key}");
    let integration_subject = format!("client-{key}");
    let flow_hash: Vec<u8> = Sha256::digest(format!("flow-{key}").as_bytes()).to_vec();
    for table in [
        "access_session",
        "access_integration_credential",
        "access_grant",
    ] {
        let query = format!(
            "DELETE FROM litecr.{table} WHERE issuer_url = $1 AND external_subject IN ($2, $3)"
        );
        sqlx::query(&query)
            .bind("https://synthetic-idp.example.test")
            .bind(&human_subject)
            .bind(&integration_subject)
            .execute(&pool)
            .await
            .expect("only this test's exact actor suffix is cleaned");
    }
    sqlx::query(
        "DELETE FROM litecr.access_audit WHERE actor_issuer_url = $1 AND actor_id IN ($2, $3)",
    )
    .bind("https://synthetic-idp.example.test")
    .bind(&human_subject)
    .bind(&integration_subject)
    .execute(&pool)
    .await
    .expect("exact-suffix audit rows cleaned");
    sqlx::query(
        "DELETE FROM litecr.access_identity WHERE issuer_url = $1 AND external_subject IN ($2, $3)",
    )
    .bind("https://synthetic-idp.example.test")
    .bind(&human_subject)
    .bind(&integration_subject)
    .execute(&pool)
    .await
    .expect("exact-suffix identities cleaned");
    sqlx::query("DELETE FROM litecr.oidc_login_transaction WHERE transaction_hash = $1")
        .bind(&flow_hash)
        .execute(&pool)
        .await
        .expect("exact-suffix OIDC flow cleaned");
    let retained: i64 = sqlx::query_scalar(
        "SELECT (SELECT COUNT(*) FROM litecr.access_identity WHERE external_subject IN ($1, $2)) + \
         (SELECT COUNT(*) FROM litecr.access_session WHERE external_subject IN ($1, $2)) + \
         (SELECT COUNT(*) FROM litecr.access_integration_credential WHERE external_subject IN ($1, $2)) + \
         (SELECT COUNT(*) FROM litecr.access_grant WHERE external_subject IN ($1, $2)) + \
         (SELECT COUNT(*) FROM litecr.access_audit WHERE actor_id IN ($1, $2)) + \
         (SELECT COUNT(*) FROM litecr.oidc_login_transaction WHERE transaction_hash = $3)",
    )
    .bind(&human_subject)
    .bind(&integration_subject)
    .bind(flow_hash)
    .fetch_one(&pool)
    .await
    .expect("test-owned row postcondition queried");
    assert_eq!(retained, 0, "test-owned access rows all removed");
    pool.close().await;
}

async fn run_access_scenario(database_url: String, key: String) {
    let pool = connect(&database_url, 4)
        .await
        .expect("dedicated access DB reachable");
    migrate(&pool).await.expect("all embedded migrations apply");

    let issuer = "https://synthetic-idp.example.test";
    let human_subject = format!("human-{key}");
    let integration_subject = format!("client-{key}");
    for (kind, subject) in [
        ("human", &human_subject),
        ("integration_client", &integration_subject),
    ] {
        sqlx::query(
            "INSERT INTO litecr.access_identity (actor_kind, issuer_url, external_subject) VALUES ($1, $2, $3)",
        )
        .bind(kind)
        .bind(issuer)
        .bind(subject)
        .execute(&pool)
        .await
        .expect("synthetic issuer-scoped identity inserted");
    }
    for (kind, subject, role, organization) in [
        ("human", &human_subject, "registrar", "org-east"),
        (
            "integration_client",
            &integration_subject,
            "integration_client",
            "org-east",
        ),
    ] {
        sqlx::query(
            "INSERT INTO litecr.access_grant (actor_kind, issuer_url, external_subject, registry_id, organization_id, record_ids, role_name, permission_name) VALUES ($1, $2, $3, 'registry-synth', $4, NULL, $5, $6)",
        )
        .bind(kind)
        .bind(issuer)
        .bind(subject)
        .bind(organization)
        .bind(role)
        .bind(Option::<String>::None)
        .execute(&pool)
        .await
        .expect("synthetic scoped grant inserted");
    }

    let directory = PostgresAccessDirectory::new(pool.clone());
    let sessions = PostgresAccessSessionRepository::new(pool.clone(), directory.clone());
    let actor = ActorRef {
        kind: ActorKind::Human,
        id: human_subject.clone(),
        issuer_url: Some(issuer.to_owned()),
    };
    let directory_identity = directory
        .load_identity(ActorKind::Human, Some(issuer), &human_subject)
        .await
        .expect("identity directory query succeeds");
    assert!(
        directory_identity
            .as_ref()
            .is_some_and(|identity| !identity.grants.is_empty()),
        "synthetic human identity and grant resolve"
    );
    let session = sessions
        .create(
            &actor,
            Some("synthetic-sso-session"),
            600,
            "test-session-create",
        )
        .await
        .expect("preprovisioned human can receive session");
    assert!(
        session.opaque_token.len() >= 43,
        "session bearer has at least 256 random bits"
    );
    assert!(
        session.csrf_token.len() >= 43,
        "CSRF token has at least 256 random bits"
    );
    let session_hash: Vec<u8> = Sha256::digest(session.opaque_token.as_bytes()).to_vec();
    let stored_session: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM litecr.access_session WHERE token_hash = $1 AND revoked_at IS NULL AND expires_at > now())")
        .bind(session_hash)
        .fetch_one(&pool)
        .await
        .expect("session query succeeds");
    assert!(
        stored_session,
        "issued opaque token hash matches the persisted session"
    );
    let loaded = sessions
        .resolve(&session.opaque_token)
        .await
        .expect("session lookup works")
        .expect("session remains active");
    assert_eq!(loaded.actor, actor);
    // Keep the credential/session DB available while the independent directory
    // pool is closed. This deterministically fails at identity reload, rather
    // than at the initial credential lookup or by timing a pool-pressure race.
    let unavailable_directory_pool = sqlx::postgres::PgPoolOptions::new()
        .connect_lazy(&database_url)
        .expect("synthetic directory pool configuration valid");
    unavailable_directory_pool.close().await;
    let unavailable_sessions = PostgresAccessSessionRepository::new(
        pool.clone(),
        PostgresAccessDirectory::new(unavailable_directory_pool),
    );
    assert!(matches!(
        unavailable_sessions.resolve(&session.opaque_token).await,
        Err(SessionError::Unavailable)
    ));
    let unavailable_authenticator = PostgresAccessAuthenticator::new(unavailable_sessions);
    assert!(matches!(
        unavailable_authenticator
            .authenticate_cookie(
                &session.opaque_token,
                None,
                false,
                "test-directory-unavailable"
            )
            .await,
        Err(SessionError::Unavailable)
    ));
    assert!(matches!(
        unavailable_authenticator
            .authenticate_cookie(&"x".repeat(43), None, false, "test-missing-session")
            .await,
        Err(SessionError::Invalid)
    ));
    sqlx::query("UPDATE litecr.access_identity SET is_active=FALSE WHERE actor_kind='human' AND issuer_url=$1 AND external_subject=$2")
        .bind(issuer).bind(&human_subject).execute(&pool).await.expect("synthetic human deactivated");
    assert!(matches!(
        sessions.resolve(&session.opaque_token).await,
        Err(SessionError::Invalid)
    ));
    sqlx::query("UPDATE litecr.access_identity SET is_active=TRUE WHERE actor_kind='human' AND issuer_url=$1 AND external_subject=$2")
        .bind(issuer).bind(&human_subject).execute(&pool).await.expect("synthetic human reactivated");
    assert!(
        sessions
            .verify_csrf(&loaded.session_id, &session.csrf_token)
            .await
            .expect("CSRF lookup works")
    );
    assert!(
        !sessions
            .verify_csrf(&loaded.session_id, "wrong-csrf-token-that-is-long-enough")
            .await
            .expect("bad CSRF checked")
    );

    let flow_store = PostgresOidcFlowStore::new(pool.clone());
    let flow = OidcFlowSecrets {
        flow_id: format!("flow-{key}"),
        state: format!("state-{key}-with-high-entropy-synthetic-value"),
        nonce: format!("nonce-{key}-with-high-entropy-synthetic-value"),
        pkce_verifier: format!("verifier-{key}-with-high-entropy-synthetic-value-padding"),
        redirect_uri: "http://127.0.0.1:18080/api/v1/access/oidc/callback".to_owned(),
    };
    flow_store.create(&flow, 300).await.expect("flow persisted");
    assert!(
        flow_store
            .consume(&flow.flow_id, "incorrect-state-value-with-enough-length")
            .await
            .expect("wrong state check succeeds")
            .is_none()
    );
    let consumed = flow_store
        .consume(&flow.flow_id, &flow.state)
        .await
        .expect("matching state check succeeds")
        .expect("flow consumed once");
    assert_eq!(consumed.authorization.nonce, flow.nonce);
    assert_eq!(consumed.authorization.pkce_verifier, flow.pkce_verifier);
    assert!(
        flow_store
            .consume(&flow.flow_id, &flow.state)
            .await
            .expect("second consume succeeds as rejection")
            .is_none()
    );
    let flow_hash: Vec<u8> = Sha256::digest(flow.flow_id.as_bytes()).to_vec();
    let flow_row = sqlx::query("SELECT nonce, pkce_verifier, consumed_at IS NOT NULL AS consumed FROM litecr.oidc_login_transaction WHERE transaction_hash = $1")
        .bind(&flow_hash)
        .fetch_one(&pool)
        .await
        .expect("flow row exists");
    assert!(flow_row.get::<Option<String>, _>("nonce").is_none());
    assert!(flow_row.get::<Option<String>, _>("pkce_verifier").is_none());
    assert!(flow_row.get::<bool, _>("consumed"));

    let raw_token = session.opaque_token.clone();
    sessions
        .revoke(&session.opaque_token, "test-session-revoke")
        .await
        .expect("session revoked");
    assert!(
        sessions
            .resolve(&raw_token)
            .await
            .expect("next request resolves persisted state")
            .is_none()
    );

    let client = sessions
        .issue_integration_credential(Some(issuer), &integration_subject, Some(600))
        .await
        .expect("preprovisioned client receives opaque credential");
    assert!(
        client.len() >= 43,
        "machine credential has at least 256 random bits"
    );
    let authenticator = PostgresAccessAuthenticator::new(sessions.clone());
    let identity = authenticator
        .authenticate_integration_credential(&client, "test-client-auth")
        .await
        .expect("machine credential authenticates");
    assert_eq!(identity.actor.kind, ActorKind::IntegrationClient);
    assert_eq!(identity.grants[0].roles, vec![Role::IntegrationClient]);
    assert!(matches!(
        unavailable_authenticator
            .authenticate_integration_credential(&client, "test-client-directory-unavailable")
            .await,
        Err(SessionError::Unavailable)
    ));
    assert!(matches!(
        unavailable_authenticator
            .authenticate_integration_credential(&"x".repeat(43), "test-missing-client")
            .await,
        Err(SessionError::Invalid)
    ));
    sqlx::query("UPDATE litecr.access_identity SET is_active=FALSE WHERE actor_kind='integration_client' AND issuer_url=$1 AND external_subject=$2")
        .bind(issuer).bind(&integration_subject).execute(&pool).await.expect("synthetic client deactivated");
    assert!(matches!(
        authenticator
            .authenticate_integration_credential(&client, "test-client-inactive")
            .await,
        Err(SessionError::Invalid)
    ));
    sqlx::query("UPDATE litecr.access_identity SET is_active=TRUE WHERE actor_kind='integration_client' AND issuer_url=$1 AND external_subject=$2")
        .bind(issuer).bind(&integration_subject).execute(&pool).await.expect("synthetic client reactivated");
    assert!(
        sessions
            .revoke_integration_credential(&client, "test-client-revoke")
            .await
            .expect("credential revoked")
    );
    assert!(
        authenticator
            .authenticate_integration_credential(&client, "test-client-after-revoke")
            .await
            .is_err()
    );

    let audit_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM litecr.access_audit WHERE actor_issuer_url = $1 AND actor_id IN ($2, $3)",
    )
    .bind(issuer)
    .bind(&human_subject)
    .bind(&integration_subject)
    .fetch_one(&pool)
    .await
    .expect("audit events queried");
    assert!(
        audit_count >= 4,
        "creation, session revoke, client auth and client revoke are audited"
    );

    pool.close().await;
}
