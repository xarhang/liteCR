//! Protected advisory matching endpoint; it returns evidence only.
use axum::{
    Extension, Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::IntoResponse,
    routing::{get, post},
};
use litecr_application::{
    access::AccessContext,
    matching::{MatchingError, MatchingService, MatchingStoreError},
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Clone)]
struct StateData {
    service: Arc<MatchingService>,
}
#[derive(Debug, Deserialize)]
struct MatchRequest {
    registry_id: String,
    organization_id: String,
    query_source_record_id: String,
    #[serde(default = "default_limit")]
    candidate_limit: usize,
}
#[derive(Debug, Deserialize)]
struct EvidenceQuery {
    registry_id: String,
    organization_id: String,
}
fn default_limit() -> usize {
    20
}
#[derive(Serialize)]
struct Envelope<T> {
    data: T,
    meta: Meta,
}
#[derive(Serialize)]
struct Meta {
    api_version: &'static str,
    cache_control: &'static str,
}
#[derive(Serialize)]
struct ErrorEnvelope {
    error: ErrorBody,
}
#[derive(Serialize)]
struct ErrorBody {
    code: &'static str,
    message: &'static str,
    retryable: bool,
}

pub fn router(service: Arc<MatchingService>) -> Router {
    Router::new()
        .route("/api/v1/match", post(evaluate))
        .route(
            "/api/v1/match/evidence/{evidence_run_id}",
            get(read_evidence),
        )
        .with_state(StateData { service })
}

async fn read_evidence(
    ctx: Option<Extension<AccessContext>>,
    State(state): State<StateData>,
    Path(evidence_run_id): Path<String>,
    Query(query): Query<EvidenceQuery>,
) -> Result<impl IntoResponse, MatchApiError> {
    let result = state
        .service
        .read_evidence(
            ctx.as_ref().map(|Extension(x)| x),
            &query.registry_id,
            &query.organization_id,
            &evidence_run_id,
        )
        .await
        .map_err(MatchApiError::from)?;
    Ok((
        [(header::CACHE_CONTROL, HeaderValue::from_static("no-store"))],
        Json(Envelope {
            data: result,
            meta: Meta {
                api_version: "v1",
                cache_control: "no-store",
            },
        }),
    ))
}

async fn evaluate(
    ctx: Option<Extension<AccessContext>>,
    State(state): State<StateData>,
    headers: HeaderMap,
    Json(request): Json<MatchRequest>,
) -> Result<impl IntoResponse, MatchApiError> {
    let idempotency_key = match idempotency_key(&headers) {
        Ok(key) => key,
        Err(()) => return Err(MatchApiError::invalid_idempotency_key()),
    };
    let result = state
        .service
        .evaluate(
            ctx.as_ref().map(|Extension(x)| x),
            &request.registry_id,
            &request.organization_id,
            &request.query_source_record_id,
            request.candidate_limit,
            idempotency_key,
        )
        .await
        .map_err(MatchApiError::from)?;
    Ok((
        [(header::CACHE_CONTROL, HeaderValue::from_static("no-store"))],
        Json(Envelope {
            data: result,
            meta: Meta {
                api_version: "v1",
                cache_control: "no-store",
            },
        }),
    ))
}

fn idempotency_key(headers: &HeaderMap) -> Result<Option<&str>, ()> {
    let mut values = headers.get_all("idempotency-key").iter();
    let Some(value) = values.next() else {
        return Ok(None);
    };
    if values.next().is_some() {
        return Err(());
    }
    let value = value.to_str().map_err(|_| ())?;
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~'))
    {
        return Err(());
    }
    Ok(Some(value))
}

struct MatchApiError {
    status: StatusCode,
    code: &'static str,
    message: &'static str,
    retryable: bool,
}
impl From<MatchingError> for MatchApiError {
    fn from(e: MatchingError) -> Self {
        match e {
            MatchingError::AccessDenied(_) => Self {
                status: StatusCode::FORBIDDEN,
                code: "ACCESS_DENIED",
                message: "The requested operation is not permitted.",
                retryable: false,
            },
            MatchingError::InvalidRequest => Self {
                status: StatusCode::UNPROCESSABLE_ENTITY,
                code: "INVALID_MATCH_REQUEST",
                message: "The matching request is invalid.",
                retryable: false,
            },
            MatchingError::InvalidIdempotencyKey => Self::invalid_idempotency_key(),
            MatchingError::Store(MatchingStoreError::NotFound) => Self {
                status: StatusCode::NOT_FOUND,
                code: "SOURCE_RECORD_NOT_FOUND",
                message: "The requested source record was not found.",
                retryable: false,
            },
            MatchingError::Store(MatchingStoreError::EvidenceNotFound) => Self {
                status: StatusCode::NOT_FOUND,
                code: "MATCH_EVIDENCE_NOT_FOUND",
                message: "The requested matching evidence was not found.",
                retryable: false,
            },
            MatchingError::Store(MatchingStoreError::StaleSnapshot) => Self {
                status: StatusCode::CONFLICT,
                code: "MATCH_SNAPSHOT_STALE",
                message: "A source record changed during matching. Request matching again.",
                retryable: false,
            },
            MatchingError::Store(MatchingStoreError::TooManyRecords) => Self {
                status: StatusCode::UNPROCESSABLE_ENTITY,
                code: "MATCH_SCOPE_TOO_LARGE",
                message: "The authorized comparison scope exceeds the matching bound.",
                retryable: false,
            },
            MatchingError::Store(MatchingStoreError::Unavailable) => Self {
                status: StatusCode::SERVICE_UNAVAILABLE,
                code: "MATCHING_UNAVAILABLE",
                message: "Matching is temporarily unavailable.",
                retryable: true,
            },
            MatchingError::Store(MatchingStoreError::IdempotencyConflict) => Self {
                status: StatusCode::CONFLICT,
                code: "IDEMPOTENCY_KEY_REUSED",
                message: "The idempotency key was already used for a different request.",
                retryable: false,
            },
        }
    }
}

impl MatchApiError {
    fn invalid_idempotency_key() -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code: "INVALID_IDEMPOTENCY_KEY",
            message: "The idempotency key is invalid.",
            retryable: false,
        }
    }
}

impl IntoResponse for MatchApiError {
    fn into_response(self) -> axum::response::Response {
        (
            self.status,
            [(header::CACHE_CONTROL, "no-store")],
            Json(ErrorEnvelope {
                error: ErrorBody {
                    code: self.code,
                    message: self.message,
                    retryable: self.retryable,
                },
            }),
        )
            .into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use litecr_application::matching::{MatchIdempotency, MatchingStore};
    use litecr_matching::MatchResult;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tower::ServiceExt;

    struct Store(Arc<AtomicUsize>);
    #[async_trait]
    impl MatchingStore for Store {
        async fn evaluate(
            &self,
            _: &str,
            _: &str,
            _: &str,
            _: usize,
            _: &str,
            _: &str,
            _: Option<MatchIdempotency>,
        ) -> Result<MatchResult, MatchingStoreError> {
            unreachable!("unauthenticated API request must not reach persistence")
        }

        async fn read_evidence(
            &self,
            _: &str,
            _: &str,
            _: &str,
        ) -> Result<MatchResult, MatchingStoreError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Err(MatchingStoreError::EvidenceNotFound)
        }
    }

    #[test]
    fn stale_snapshot_maps_to_a_safe_conflict_response() {
        let error = MatchApiError::from(MatchingError::Store(MatchingStoreError::StaleSnapshot));

        assert_eq!(error.status, StatusCode::CONFLICT);
        assert_eq!(error.code, "MATCH_SNAPSHOT_STALE");
        assert!(!error.retryable);
        assert!(!error.message.contains("source_record_id"));
    }

    #[tokio::test]
    async fn evidence_route_requires_authorized_context_before_storage_access() {
        let calls = Arc::new(AtomicUsize::new(0));
        let service = Arc::new(MatchingService::new(Box::new(Store(calls.clone())), 100).unwrap());
        let response = router(service)
            .oneshot(
                axum::http::Request::builder()
                    .uri("/api/v1/match/evidence/00000000-0000-0000-0000-000000000001?registry_id=r&organization_id=o")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn scoped_missing_evidence_maps_to_non_disclosing_not_found() {
        let error = MatchApiError::from(MatchingError::Store(MatchingStoreError::EvidenceNotFound));
        assert_eq!(error.status, StatusCode::NOT_FOUND);
        assert_eq!(error.code, "MATCH_EVIDENCE_NOT_FOUND");
        assert!(!error.message.contains("registry"));
        assert!(!error.retryable);
    }

    #[test]
    fn idempotency_header_accepts_only_one_bounded_ascii_token() {
        let mut headers = HeaderMap::new();
        assert_eq!(idempotency_key(&headers).unwrap(), None);
        headers.insert("idempotency-key", HeaderValue::from_static("request_01~a"));
        assert_eq!(idempotency_key(&headers).unwrap(), Some("request_01~a"));
        headers.insert(
            "idempotency-key",
            HeaderValue::from_bytes(&[b'a'; 129]).unwrap(),
        );
        assert!(idempotency_key(&headers).is_err());
        headers.clear();
        headers.insert("idempotency-key", HeaderValue::from_static("bad key"));
        assert!(idempotency_key(&headers).is_err());
        headers.insert("idempotency-key", HeaderValue::from_static("second"));
        headers.append("idempotency-key", HeaderValue::from_static("third"));
        assert!(idempotency_key(&headers).is_err());
    }

    #[test]
    fn reused_idempotency_key_conflict_has_stable_safe_response() {
        let error = MatchApiError::from(MatchingError::Store(
            MatchingStoreError::IdempotencyConflict,
        ));
        assert_eq!(error.status, StatusCode::CONFLICT);
        assert_eq!(error.code, "IDEMPOTENCY_KEY_REUSED");
        assert!(!error.retryable);
        assert!(!error.message.contains("record"));
    }
}
