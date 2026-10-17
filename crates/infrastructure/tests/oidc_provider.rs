//! End-to-end OIDC negative checks against a loopback-only synthetic provider.
//! The embedded private key is public test material and must never be reused.

use axum::{
    Router,
    body::Bytes,
    extract::State,
    http::StatusCode,
    routing::{get, post},
};
use chrono::{Duration, Utc};
use litecr_application::access::{OidcFlow, OidcProvider, OidcProviderError};
use litecr_infrastructure::access::{OidcProviderClient, OidcProviderConfig};
use openidconnect::{
    AccessToken, Audience, IssuerUrl, JsonWebKeyId, Nonce, PrivateSigningKey, StandardClaims,
    SubjectIdentifier,
    core::{
        CoreIdToken, CoreIdTokenClaims, CoreJsonWebKeySet, CoreJwsSigningAlgorithm,
        CoreRsaPrivateSigningKey,
    },
};
use serde_json::{Value, json};
use std::time::Duration as StdDuration;
use tokio::{net::TcpListener, task::JoinHandle};

const CLIENT_ID: &str = "litecr-synthetic-web";
const NONCE: &str = "synthetic-nonce-from-login-flow";
const PRIVATE_KEY: &str = include_str!("fixtures/oidc-test-rsa.pem");

#[derive(Clone)]
struct ProviderState {
    discovery: Value,
    jwks: Value,
    token: Value,
    reject_token: bool,
}

async fn discovery_handler(State(state): State<ProviderState>) -> axum::Json<Value> {
    axum::Json(state.discovery)
}

async fn jwks_handler(State(state): State<ProviderState>) -> axum::Json<Value> {
    axum::Json(state.jwks)
}

async fn token_handler(
    State(state): State<ProviderState>,
    _body: Bytes,
) -> (StatusCode, axum::Json<Value>) {
    let status = if state.reject_token {
        StatusCode::BAD_REQUEST
    } else {
        StatusCode::OK
    };
    (status, axum::Json(state.token))
}

struct LocalProvider {
    issuer: String,
    server: JoinHandle<()>,
}

impl Drop for LocalProvider {
    fn drop(&mut self) {
        self.server.abort();
    }
}

async fn start_provider(
    claims_issuer_override: Option<&str>,
    audience: &str,
    expires_in: Duration,
    nonce: &str,
    bad_signature: bool,
    reject_token: bool,
) -> LocalProvider {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let issuer = format!("http://{addr}/");
    let issuer_root = issuer.trim_end_matches('/');
    let signing_key = CoreRsaPrivateSigningKey::from_pem(
        PRIVATE_KEY,
        Some(JsonWebKeyId::new("synthetic-test-key".to_owned())),
    )
    .expect("embedded synthetic RSA fixture parses");
    let jwks_doc = CoreJsonWebKeySet::new(vec![signing_key.as_verification_key()]);
    let id_token = CoreIdToken::new(
        CoreIdTokenClaims::new(
            IssuerUrl::new(claims_issuer_override.unwrap_or(&issuer).to_owned()).unwrap(),
            vec![Audience::new(audience.to_owned())],
            Utc::now() + expires_in,
            Utc::now(),
            StandardClaims::new(SubjectIdentifier::new("synthetic-subject".to_owned())),
            Default::default(),
        )
        .set_nonce(Some(Nonce::new(nonce.to_owned()))),
        &signing_key,
        CoreJwsSigningAlgorithm::RsaSsaPkcs1V15Sha256,
        Some(&AccessToken::new("synthetic-access-token".to_owned())),
        None,
    )
    .expect("synthetic ID token signs");
    let mut compact = id_token.to_string();
    if bad_signature {
        let signature_start = compact.rfind('.').expect("ID token has a signature") + 1;
        let first_signature_char = compact.as_bytes()[signature_start] as char;
        let replacement = if first_signature_char == 'A' {
            'B'
        } else {
            'A'
        };
        compact.replace_range(
            signature_start..signature_start + 1,
            &replacement.to_string(),
        );
    }
    let token_doc = json!({
        "access_token": "synthetic-access-token",
        "token_type": "Bearer",
        "expires_in": 300,
        "id_token": compact,
    });
    let state = ProviderState {
        discovery: json!({
            "issuer": issuer,
            "authorization_endpoint": format!("{issuer_root}/authorize"),
            "token_endpoint": format!("{issuer_root}/token"),
            "jwks_uri": format!("{issuer_root}/jwks"),
            "response_types_supported": ["code"],
            "subject_types_supported": ["public"],
            "id_token_signing_alg_values_supported": ["RS256"],
            "scopes_supported": ["openid"],
            "token_endpoint_auth_methods_supported": ["none"],
        }),
        jwks: serde_json::to_value(jwks_doc).unwrap(),
        token: token_doc,
        reject_token,
    };
    let app = Router::new()
        .route("/.well-known/openid-configuration", get(discovery_handler))
        .route("/jwks", get(jwks_handler))
        .route("/token", post(token_handler))
        .with_state(state);
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    // Ensure the listener has entered serve before metadata discovery begins.
    tokio::time::sleep(StdDuration::from_millis(10)).await;
    LocalProvider { issuer, server }
}

fn config(issuer: String) -> OidcProviderConfig {
    OidcProviderConfig {
        issuer_url: issuer,
        client_id: CLIENT_ID.to_owned(),
        client_secret: None,
        redirect_uri: "https://litecr.example.test/callback".to_owned(),
        allow_http_loopback: true,
    }
}

fn flow() -> OidcFlow {
    litecr_application::access::OidcFlow {
        flow_id: "synthetic-flow-id".to_owned(),
        authorization: litecr_application::access::OidcAuthorization {
            flow_id: "synthetic-flow-id".to_owned(),
            authorization_url: String::new(),
            redirect_uri: "https://litecr.example.test/callback".to_owned(),
            state: "synthetic-state".to_owned(),
            nonce: NONCE.to_owned(),
            pkce_verifier: "synthetic-pkce-verifier-with-sufficient-length".to_owned(),
        },
    }
}

async fn assert_rejected(provider: LocalProvider, label: &str) {
    let client = OidcProviderClient::discover(config(provider.issuer.clone()))
        .await
        .expect("synthetic loopback discovery succeeds");
    let result = client.exchange_code("synthetic-code", &flow()).await;
    assert!(
        matches!(result, Err(OidcProviderError::Rejected)),
        "provider must reject {label} token fixture, got {result:?}"
    );
}

#[tokio::test]
async fn signed_oidc_tokens_reject_signature_issuer_audience_expiry_and_nonce_failures() {
    let cases = [
        (
            "bad signature",
            None,
            CLIENT_ID,
            Duration::minutes(5),
            NONCE,
            true,
        ),
        (
            "wrong issuer",
            Some("https://attacker.invalid"),
            CLIENT_ID,
            Duration::minutes(5),
            NONCE,
            false,
        ),
        (
            "wrong audience",
            None,
            "another-client",
            Duration::minutes(5),
            NONCE,
            false,
        ),
        (
            "expired token",
            None,
            CLIENT_ID,
            Duration::minutes(-5),
            NONCE,
            false,
        ),
        (
            "wrong nonce",
            None,
            CLIENT_ID,
            Duration::minutes(5),
            "attacker-nonce",
            false,
        ),
    ];
    for (label, claims_issuer, audience, expiry, nonce, bad_signature) in cases {
        let provider =
            start_provider(claims_issuer, audience, expiry, nonce, bad_signature, false).await;
        assert_rejected(provider, label).await;
    }
}

#[tokio::test]
async fn token_endpoint_rejection_fails_closed() {
    let provider = start_provider(None, CLIENT_ID, Duration::minutes(5), NONCE, false, true).await;
    assert_rejected(provider, "token endpoint rejection").await;
}
