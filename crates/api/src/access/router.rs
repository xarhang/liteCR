use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use axum::{
    Json, Router,
    body::Body,
    extract::{Query, State},
    http::{HeaderMap, HeaderValue, Method, Request, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Redirect, Response},
    routing::{get, post},
};
use litecr_application::access::{
    AccessContext, AccessService, AccessServiceError, Permission, ResourceScope,
    SecurityAuditEvent, SecurityEventReason, SecurityEventType, SecurityRouteFamily,
    SecurityScopeShape, SessionError, authorize,
};
use serde::{Deserialize, Serialize};

const SESSION_COOKIE: &str = "litecr_session";
const CSRF_COOKIE: &str = "litecr_csrf";
const FLOW_COOKIE: &str = "litecr_oidc_flow";
static REQUEST_SEQUENCE: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone)]
pub struct AccessApiConfig {
    pub expected_origin: String,
    pub secure_cookies: bool,
    pub environment: AccessEnvironment,
    pub session_lifetime_seconds: i64,
    pub flow_lifetime_seconds: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessEnvironment {
    Production,
    Development,
}

impl AccessApiConfig {
    pub fn validate(&self) -> bool {
        let https_authority = self.expected_origin.strip_prefix("https://");
        let http_authority = self.expected_origin.strip_prefix("http://");
        let authority = https_authority.or(http_authority).unwrap_or_default();
        let local_http = http_authority.is_some() && is_loopback_authority(authority);
        ((https_authority.is_some() && self.secure_cookies)
            || (self.environment == AccessEnvironment::Development
                && local_http
                && !self.secure_cookies))
            && !authority.is_empty()
            && valid_origin_authority(authority)
            && self.session_lifetime_seconds > 0
            && self.session_lifetime_seconds <= 86_400
            && self.flow_lifetime_seconds > 0
            && self.flow_lifetime_seconds <= 900
    }
}

fn is_loopback_authority(authority: &str) -> bool {
    let host = authority
        .strip_prefix('[')
        .and_then(|remaining| remaining.split_once(']').map(|(host, _)| host))
        .or_else(|| authority.split_once(':').map(|(host, _)| host))
        .unwrap_or(authority);
    host == "localhost"
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|address| address.is_loopback())
}

fn valid_origin_authority(authority: &str) -> bool {
    if authority.is_empty() || authority.contains(['\r', '\n', '/', '?', '#', '@', ' ']) {
        return false;
    }
    let (host, port) = if let Some(bracketed) = authority.strip_prefix('[') {
        let Some((host, suffix)) = bracketed.split_once(']') else {
            return false;
        };
        if host.parse::<std::net::Ipv6Addr>().is_err() {
            return false;
        }
        if suffix.is_empty() {
            (host, None)
        } else if let Some(port) = suffix.strip_prefix(':') {
            (host, Some(port))
        } else {
            return false;
        }
    } else if let Some((host, port)) = authority.split_once(':') {
        if host.contains(':') {
            return false;
        }
        (host, Some(port))
    } else {
        (authority, None)
    };
    if let Some(port) = port {
        if port.is_empty()
            || !port.bytes().all(|byte| byte.is_ascii_digit())
            || port.parse::<u16>().is_err()
            || port.parse::<u16>().is_ok_and(|value| value == 0)
        {
            return false;
        }
    }
    if host.parse::<std::net::IpAddr>().is_ok() {
        return true;
    }
    !host.is_empty()
        && host.len() <= 253
        && host.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && label.as_bytes()[0].is_ascii_alphanumeric()
                && label.as_bytes()[label.len() - 1].is_ascii_alphanumeric()
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
}

#[derive(Clone)]
struct ApiState {
    service: Arc<AccessService>,
    config: AccessApiConfig,
}

#[derive(Clone)]
struct ProtectedState {
    service: Arc<AccessService>,
    expected_origin: String,
}

#[derive(Debug, Deserialize)]
struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

#[derive(Debug, Serialize)]
struct ErrorBody {
    code: &'static str,
    message: &'static str,
}

#[derive(Debug, Serialize)]
struct SessionBody {
    actor_kind: &'static str,
    actor_id: String,
    request_id: String,
}

pub fn router(service: Arc<AccessService>, config: AccessApiConfig) -> Router {
    assert!(
        config.validate(),
        "invalid access API security configuration"
    );
    let state = ApiState { service, config };
    Router::new()
        .route("/api/v1/access/oidc/login", get(begin_login))
        .route("/api/v1/access/oidc/callback", get(complete_login))
        .route("/api/v1/access/session", get(current_session))
        .route("/api/v1/access/logout", post(logout))
        .layer(middleware::from_fn(access_response_headers))
        .with_state(state)
}

async fn access_response_headers(request: Request<Body>, next: Next) -> Response {
    no_store(next.run(request).await)
}

/// Apply to protected feature routers. Health endpoints should remain outside this layer.
pub fn protected_routes(
    router: Router,
    service: Arc<AccessService>,
    config: AccessApiConfig,
) -> Router {
    assert!(
        config.validate(),
        "invalid protected-route security configuration"
    );
    router.layer(middleware::from_fn_with_state(
        ProtectedState {
            service,
            expected_origin: config.expected_origin,
        },
        require_session,
    ))
}

async fn begin_login(State(state): State<ApiState>) -> Response {
    match state.service.begin_login().await {
        Ok(challenge) => {
            let cookie = cookie(
                FLOW_COOKIE,
                &challenge.flow_cookie,
                &format!(
                    "Path=/api/v1/access/oidc/callback; Max-Age={}; HttpOnly; SameSite=Lax",
                    state.config.flow_lifetime_seconds
                ),
                state.config.secure_cookies,
            );
            match cookie {
                Ok(cookie) => {
                    let mut response =
                        Redirect::temporary(&challenge.authorization_url).into_response();
                    response.headers_mut().append(header::SET_COOKIE, cookie);
                    response
                }
                Err(_) => internal_error(),
            }
        }
        Err(AccessServiceError::Unavailable) => internal_error(),
        Err(_) => unauthorized(),
    }
}

async fn complete_login(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Query(query): Query<CallbackQuery>,
) -> Response {
    let rid = request_id(&headers);
    if query.error.is_some() {
        return callback_denied(&state, SecurityEventReason::InvalidCallback, &rid).await;
    }
    let Some(code) = query.code.as_deref() else {
        return callback_denied(&state, SecurityEventReason::InvalidCallback, &rid).await;
    };
    let Some(oidc_state) = query.state.as_deref() else {
        return callback_denied(&state, SecurityEventReason::InvalidCallback, &rid).await;
    };
    let Some(flow_cookie) = cookie_value(&headers, FLOW_COOKIE) else {
        return callback_denied(&state, SecurityEventReason::InvalidCallback, &rid).await;
    };
    if code.len() > 4096 || oidc_state.len() > 512 || flow_cookie.len() > 256 {
        return callback_denied(&state, SecurityEventReason::InvalidCallback, &rid).await;
    }
    match state
        .service
        .complete_login(&flow_cookie, oidc_state, code, &rid)
        .await
    {
        Ok(result) => {
            let secure = state.config.secure_cookies;
            let session_cookie = cookie(
                SESSION_COOKIE,
                &result.session.opaque_token,
                &format!(
                    "Path=/api/v1; Max-Age={}; HttpOnly; SameSite=Lax",
                    state.config.session_lifetime_seconds
                ),
                secure,
            );
            let csrf_cookie = cookie(
                CSRF_COOKIE,
                &result.session.csrf_token,
                &format!(
                    // The root-served SPA reads this non-HttpOnly double-submit token.
                    // Keep the session cookie API-scoped, but expose CSRF on the app path.
                    "Path=/; Max-Age={}; SameSite=Strict",
                    state.config.session_lifetime_seconds
                ),
                secure,
            );
            match (session_cookie, csrf_cookie) {
                (Ok(session_cookie), Ok(csrf_cookie)) => {
                    let mut response = Redirect::temporary("/").into_response();
                    response
                        .headers_mut()
                        .append(header::SET_COOKIE, session_cookie);
                    response
                        .headers_mut()
                        .append(header::SET_COOKIE, csrf_cookie);
                    clear_flow_cookie(response, &state.config)
                }
                _ => internal_error(),
            }
        }
        Err(AccessServiceError::Unavailable) => clear_flow_cookie(internal_error(), &state.config),
        Err(_) => clear_flow_cookie(unauthorized(), &state.config),
    }
}

async fn current_session(State(state): State<ApiState>, headers: HeaderMap) -> Response {
    let rid = request_id(&headers);
    let Some(session_cookie) = cookie_value(&headers, SESSION_COOKIE) else {
        return session_denied(
            &state.service,
            SecurityEventReason::MissingCredential,
            SecurityRouteFamily::Session,
            SecurityScopeShape::None,
            None,
            &rid,
        )
        .await;
    };
    match state
        .service
        .authenticate(&session_cookie, None, false, &rid)
        .await
    {
        Ok(context) => {
            let actor_kind = match context.actor().kind {
                litecr_application::access::ActorKind::Human => "human",
                litecr_application::access::ActorKind::IntegrationClient => "integration_client",
                litecr_application::access::ActorKind::System => "system",
            };
            (
                StatusCode::OK,
                Json(SessionBody {
                    actor_kind,
                    actor_id: context.actor().id.clone(),
                    request_id: rid,
                }),
            )
                .into_response()
        }
        Err(SessionError::Unavailable) => internal_error(),
        Err(_) => {
            session_denied(
                &state.service,
                SecurityEventReason::InvalidCredential,
                SecurityRouteFamily::Session,
                SecurityScopeShape::None,
                None,
                &rid,
            )
            .await
        }
    }
}

async fn logout(State(state): State<ApiState>, headers: HeaderMap) -> Response {
    let rid = request_id(&headers);
    if !origin_matches(&headers, &state.config.expected_origin) {
        return session_denied(
            &state.service,
            SecurityEventReason::CsrfRejected,
            SecurityRouteFamily::Logout,
            SecurityScopeShape::None,
            None,
            &rid,
        )
        .await;
    }
    let (Some(session_cookie), Some(csrf_cookie), Some(csrf_header)) = (
        cookie_value(&headers, SESSION_COOKIE),
        cookie_value(&headers, CSRF_COOKIE),
        headers
            .get("x-csrf-token")
            .and_then(|value| value.to_str().ok()),
    ) else {
        return session_denied(
            &state.service,
            SecurityEventReason::MissingCredential,
            SecurityRouteFamily::Logout,
            SecurityScopeShape::None,
            None,
            &rid,
        )
        .await;
    };
    if csrf_cookie != csrf_header {
        return session_denied(
            &state.service,
            SecurityEventReason::CsrfRejected,
            SecurityRouteFamily::Logout,
            SecurityScopeShape::None,
            None,
            &rid,
        )
        .await;
    }
    match state
        .service
        .authenticate(&session_cookie, Some(csrf_header), true, &rid)
        .await
    {
        Ok(_) => match state
            .service
            .logout(&session_cookie, &request_id(&headers))
            .await
        {
            Ok(()) => {
                let mut response = StatusCode::NO_CONTENT.into_response();
                if let (Ok(session), Ok(csrf)) = (
                    clear_cookie(
                        SESSION_COOKIE,
                        "Path=/api/v1; HttpOnly; SameSite=Lax",
                        state.config.secure_cookies,
                    ),
                    clear_cookie(
                        CSRF_COOKIE,
                        "Path=/; SameSite=Strict",
                        state.config.secure_cookies,
                    ),
                ) {
                    response.headers_mut().append(header::SET_COOKIE, session);
                    response.headers_mut().append(header::SET_COOKIE, csrf);
                }
                response
            }
            Err(_) => internal_error(),
        },
        Err(SessionError::Unavailable) => internal_error(),
        Err(SessionError::Invalid) => {
            session_denied(
                &state.service,
                SecurityEventReason::InvalidCredential,
                SecurityRouteFamily::Logout,
                SecurityScopeShape::None,
                None,
                &rid,
            )
            .await
        }
    }
}

async fn require_session(
    State(state): State<ProtectedState>,
    mut request: Request<Body>,
    next: Next,
) -> Response {
    let path = request.uri().path().to_owned();
    let (route_family, scope_shape) = security_route_metadata(&path);
    let headers = request.headers();
    let unsafe_method = matches!(
        *request.method(),
        Method::POST | Method::PUT | Method::PATCH | Method::DELETE
    );
    let csrf_header = headers
        .get("x-csrf-token")
        .and_then(|value| value.to_str().ok());
    let csrf_cookie = cookie_value(headers, CSRF_COOKIE);
    let rid = request_id(headers);
    let session_cookie = cookie_value(headers, SESSION_COOKIE);
    let bearer = match authorization_bearer(headers) {
        Ok(bearer) => bearer,
        Err(()) => {
            return protected_denial(
                &state.service,
                unauthorized(),
                SecurityEventReason::ConflictingCredentials,
                route_family,
                scope_shape,
                None,
                &rid,
                &path,
            )
            .await;
        }
    };
    if bearer.is_some() && session_cookie.is_some() {
        return protected_denial(
            &state.service,
            unauthorized(),
            SecurityEventReason::ConflictingCredentials,
            route_family,
            scope_shape,
            None,
            &rid,
            &path,
        )
        .await;
    }
    let authenticated = if let Some(bearer) = bearer {
        state
            .service
            .authenticate_integration_credential(bearer, &rid)
            .await
    } else {
        let Some(session_cookie) = session_cookie else {
            return protected_denial(
                &state.service,
                unauthorized(),
                SecurityEventReason::MissingCredential,
                route_family,
                scope_shape,
                None,
                &rid,
                &path,
            )
            .await;
        };
        if unsafe_method
            && (!origin_matches(headers, &state.expected_origin)
                || csrf_header.is_none()
                || csrf_cookie.as_deref() != csrf_header)
        {
            return protected_denial(
                &state.service,
                (
                    StatusCode::FORBIDDEN,
                    Json(ErrorBody {
                        code: "CSRF_REJECTED",
                        message: "Request could not be verified.",
                    }),
                )
                    .into_response(),
                SecurityEventReason::CsrfRejected,
                route_family,
                scope_shape,
                None,
                &rid,
                &path,
            )
            .await;
        }
        state
            .service
            .authenticate(&session_cookie, csrf_header, unsafe_method, &rid)
            .await
    };
    match authenticated {
        Ok(context) => {
            let actor = Some(context.actor().clone());
            request.extensions_mut().insert(context);
            request.extensions_mut().insert(rid.clone());
            let response = next.run(request).await;
            if response.status() == StatusCode::FORBIDDEN {
                protected_denial(
                    &state.service,
                    response,
                    SecurityEventReason::PermissionDenied,
                    route_family,
                    scope_shape,
                    actor,
                    &rid,
                    &path,
                )
                .await
            } else {
                protected_failure(response, &path)
            }
        }
        Err(SessionError::Unavailable) => protected_failure(internal_error(), &path),
        Err(_) => {
            protected_denial(
                &state.service,
                unauthorized(),
                SecurityEventReason::InvalidCredential,
                route_family,
                scope_shape,
                None,
                &rid,
                &path,
            )
            .await
        }
    }
}

async fn callback_denied(
    state: &ApiState,
    reason: SecurityEventReason,
    request_id: &str,
) -> Response {
    let event = SecurityAuditEvent {
        event_type: SecurityEventType::OidcCallbackDenied,
        reason,
        route_family: SecurityRouteFamily::OidcCallback,
        scope_shape: SecurityScopeShape::None,
        actor: None,
        request_id: request_id.to_owned(),
    };
    let response = if state.service.record_security_event(event).await.is_ok() {
        unauthorized()
    } else {
        internal_error()
    };
    clear_flow_cookie(response, &state.config)
}

async fn session_denied(
    service: &AccessService,
    reason: SecurityEventReason,
    route_family: SecurityRouteFamily,
    scope_shape: SecurityScopeShape,
    actor: Option<litecr_application::access::ActorRef>,
    request_id: &str,
) -> Response {
    let event = SecurityAuditEvent {
        event_type: SecurityEventType::SessionAuthenticationDenied,
        reason,
        route_family,
        scope_shape,
        actor,
        request_id: request_id.to_owned(),
    };
    if service.record_security_event(event).await.is_err() {
        internal_error()
    } else if reason == SecurityEventReason::CsrfRejected {
        (
            StatusCode::FORBIDDEN,
            Json(ErrorBody {
                code: "CSRF_REJECTED",
                message: "Request could not be verified.",
            }),
        )
            .into_response()
    } else {
        unauthorized()
    }
}

#[allow(clippy::too_many_arguments)]
async fn protected_denial(
    service: &AccessService,
    response: Response,
    reason: SecurityEventReason,
    route_family: SecurityRouteFamily,
    scope_shape: SecurityScopeShape,
    actor: Option<litecr_application::access::ActorRef>,
    request_id: &str,
    path: &str,
) -> Response {
    let event = SecurityAuditEvent {
        event_type: SecurityEventType::ProtectedRouteDenied,
        reason,
        route_family,
        scope_shape,
        actor,
        request_id: request_id.to_owned(),
    };
    if service.record_security_event(event).await.is_err() {
        protected_failure(internal_error(), path)
    } else {
        protected_failure(response, path)
    }
}

fn security_route_metadata(path: &str) -> (SecurityRouteFamily, SecurityScopeShape) {
    use SecurityRouteFamily as Route;
    use SecurityScopeShape as Scope;
    let segments = path
        .split('/')
        .filter(|segment| !segment.is_empty())
        .count();
    let (route, scope) = if path == "/fhir/metadata" || path == "/fhir/r4" {
        (Route::FhirMetadata, Scope::None)
    } else if path == "/fhir/Patient"
        || path == "/fhir/r4/Patient"
        || path.starts_with("/fhir/Patient/")
        || path.starts_with("/fhir/r4/Patient/")
    {
        (
            Route::FhirPatient,
            if path.starts_with("/fhir/Patient/") || path.starts_with("/fhir/r4/Patient/") {
                Scope::PathRecord
            } else {
                Scope::RequestBound
            },
        )
    } else if path.starts_with("/api/v1/review-cases") {
        (
            Route::Review,
            if segments >= 4 {
                Scope::PathRecord
            } else {
                Scope::RequestBound
            },
        )
    } else if path.starts_with("/api/v1/matches") {
        (Route::Matching, Scope::RequestBound)
    } else if path.starts_with("/api/v1/migrations") {
        (Route::Migration, Scope::RequestBound)
    } else if path.starts_with("/api/v1/config") {
        (Route::Configuration, Scope::RequestBound)
    } else if path.starts_with("/api/v1/access") {
        (Route::Session, Scope::None)
    } else if path.starts_with("/api/v1/source-records") || path.starts_with("/api/v1/person") {
        (Route::Registry, Scope::RequestBound)
    } else {
        (Route::OtherProtected, Scope::Unknown)
    };
    (route, scope)
}

fn protected_failure(response: Response, path: &str) -> Response {
    let status = response.status();
    let is_fhir_route = path == "/fhir/Patient"
        || path.starts_with("/fhir/Patient/")
        || path == "/fhir/metadata"
        || path == "/fhir/r4"
        || path.starts_with("/fhir/r4/");
    let already_fhir = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("application/fhir+json"));
    if is_fhir_route
        && !already_fhir
        && matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN)
    {
        crate::integrations::fhir::access_failure(status)
    } else {
        no_store(response)
    }
}

fn no_store(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

/// Reusable guard for application handlers after `protected_routes` has resolved context.
pub fn require_permission(
    context: Option<&AccessContext>,
    permission: Permission,
    scope: &ResourceScope,
) -> Result<(), StatusCode> {
    authorize(context, permission, scope).map_err(|_| StatusCode::FORBIDDEN)
}

fn request_id(headers: &HeaderMap) -> String {
    if let Some(value) = headers
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
    {
        // Only accept a canonical UUID as a caller correlation ID. Opaque
        // credentials or free-form clinical text must never become audit IDs.
        if is_canonical_uuid(value) {
            return value.to_owned();
        }
    }
    format!(
        "access-{}",
        REQUEST_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    )
}

fn is_canonical_uuid(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 36
        && [8, 13, 18, 23]
            .into_iter()
            .all(|index| bytes[index] == b'-')
        && bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| [8, 13, 18, 23].contains(&index) || byte.is_ascii_hexdigit())
}

fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    let mut found = None;
    for value in headers.get_all(header::COOKIE) {
        let value = value.to_str().ok()?;
        for segment in value.split(';') {
            let (key, value) = segment.trim().split_once('=')?;
            if key == name {
                if found.is_some()
                    || value.is_empty()
                    || !value
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || b"-._~".contains(&byte))
                {
                    return None;
                }
                found = Some(value.to_owned());
            }
        }
    }
    found
}

/// Resolve the only supported Authorization scheme without accepting ambiguous
/// duplicate header fields or silently falling back to a session cookie.
fn authorization_bearer(headers: &HeaderMap) -> Result<Option<&str>, ()> {
    let mut values = headers.get_all(header::AUTHORIZATION).iter();
    let Some(value) = values.next() else {
        return Ok(None);
    };
    if values.next().is_some() {
        return Err(());
    }
    let value = value.to_str().map_err(|_| ())?;
    let (scheme, credentials) = value.split_once(' ').ok_or(())?;
    if !scheme.eq_ignore_ascii_case("Bearer") {
        return Err(());
    }
    let bearer = credentials.trim_start_matches(' ');
    if bearer.is_empty() || bearer.bytes().any(|byte| byte.is_ascii_whitespace()) {
        return Err(());
    }
    Ok(Some(bearer))
}

fn origin_matches(headers: &HeaderMap, expected: &str) -> bool {
    headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|origin| origin == expected)
}

fn cookie(name: &str, value: &str, attributes: &str, secure: bool) -> Result<HeaderValue, ()> {
    let secure = if secure { "; Secure" } else { "" };
    HeaderValue::from_str(&format!("{name}={value}; {attributes}{secure}")).map_err(|_| ())
}

fn clear_cookie(name: &str, attributes: &str, secure: bool) -> Result<HeaderValue, ()> {
    let secure = if secure { "; Secure" } else { "" };
    HeaderValue::from_str(&format!("{name}=; {attributes}; Max-Age=0{secure}")).map_err(|_| ())
}

fn clear_flow_cookie(mut response: Response, config: &AccessApiConfig) -> Response {
    if let Ok(cookie) = clear_cookie(
        FLOW_COOKIE,
        "Path=/api/v1/access/oidc/callback; HttpOnly; SameSite=Lax",
        config.secure_cookies,
    ) {
        response.headers_mut().append(header::SET_COOKIE, cookie);
    }
    response
}

fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(ErrorBody {
            code: "UNAUTHENTICATED",
            message: "Authentication is required.",
        }),
    )
        .into_response()
}

fn internal_error() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(ErrorBody {
            code: "ACCESS_UNAVAILABLE",
            message: "Access service is temporarily unavailable.",
        }),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn access_responses_never_cache_success_errors_or_redirects() {
        use tower::ServiceExt;
        let routes = Router::new()
            .route("/session", get(|| async { StatusCode::OK }))
            .route("/denied", get(|| async { StatusCode::UNAUTHORIZED }))
            .route("/login", get(|| async { Redirect::temporary("/provider") }))
            .layer(middleware::from_fn(access_response_headers));
        for path in ["/session", "/denied", "/login"] {
            let response = routes
                .clone()
                .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(
                response.headers().get(header::CACHE_CONTROL).unwrap(),
                "no-store"
            );
        }
    }

    #[test]
    fn cookie_policy_requires_https_except_explicit_loopback_development() {
        assert!(
            AccessApiConfig {
                expected_origin: "https://litecr.example.org".to_owned(),
                secure_cookies: true,
                environment: AccessEnvironment::Production,
                session_lifetime_seconds: 28_800,
                flow_lifetime_seconds: 300,
            }
            .validate()
        );
        assert!(
            AccessApiConfig {
                expected_origin: "http://127.0.0.1:18080".to_owned(),
                secure_cookies: false,
                environment: AccessEnvironment::Development,
                session_lifetime_seconds: 28_800,
                flow_lifetime_seconds: 300,
            }
            .validate()
        );
        assert!(
            !AccessApiConfig {
                expected_origin: "http://litecr.example.org".to_owned(),
                secure_cookies: false,
                environment: AccessEnvironment::Development,
                session_lifetime_seconds: 28_800,
                flow_lifetime_seconds: 300,
            }
            .validate()
        );
        assert!(
            !AccessApiConfig {
                expected_origin: "https://litecr.example.org/evil".to_owned(),
                secure_cookies: true,
                environment: AccessEnvironment::Production,
                session_lifetime_seconds: 28_800,
                flow_lifetime_seconds: 300,
            }
            .validate()
        );
    }

    #[test]
    fn production_rejects_loopback_http_and_bad_origins_fail_validation() {
        assert!(
            !AccessApiConfig {
                expected_origin: "http://127.0.0.1:18080".to_owned(),
                secure_cookies: false,
                environment: AccessEnvironment::Production,
                session_lifetime_seconds: 28_800,
                flow_lifetime_seconds: 300,
            }
            .validate()
        );
        for origin in [
            "https://example.org/path",
            "https://user@example.org",
            "https://example.org?x",
            "not-an-origin",
            "https://host.example:bogus",
            "https://host.example:",
            "https://bad_host.example",
            "https://example..org",
        ] {
            assert!(
                !AccessApiConfig {
                    expected_origin: origin.to_owned(),
                    secure_cookies: true,
                    environment: AccessEnvironment::Production,
                    session_lifetime_seconds: 28_800,
                    flow_lifetime_seconds: 300,
                }
                .validate()
            );
        }
    }

    #[test]
    fn duplicate_or_malformed_auth_cookies_fail_closed() {
        let duplicate = HeaderMap::from_iter([(
            header::COOKIE,
            HeaderValue::from_static("litecr_session=abcDEF0123456789; litecr_session=second"),
        )]);
        assert!(cookie_value(&duplicate, SESSION_COOKIE).is_none());
        let malformed = HeaderMap::from_iter([(
            header::COOKIE,
            HeaderValue::from_static("litecr_session=bad%2Fvalue"),
        )]);
        assert!(cookie_value(&malformed, SESSION_COOKIE).is_none());
    }

    #[test]
    fn authorization_header_must_be_single_bearer_credential() {
        let mut headers = HeaderMap::new();
        assert_eq!(authorization_bearer(&headers), Ok(None));

        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer credential-one"),
        );
        assert_eq!(authorization_bearer(&headers), Ok(Some("credential-one")));

        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("bEaReR  credential-one"),
        );
        assert_eq!(authorization_bearer(&headers), Ok(Some("credential-one")));

        headers.append(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer credential-two"),
        );
        assert!(authorization_bearer(&headers).is_err());

        headers.remove(header::AUTHORIZATION);
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Basic credential"),
        );
        assert!(authorization_bearer(&headers).is_err());
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer credential with-space"),
        );
        assert!(authorization_bearer(&headers).is_err());
    }

    #[test]
    fn csrf_origin_comparison_requires_exact_origin() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::ORIGIN,
            HeaderValue::from_static("https://host.example"),
        );
        assert!(origin_matches(&headers, "https://host.example"));
        assert!(!origin_matches(&headers, "https://evil.example"));
    }
}
