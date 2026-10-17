//! Protected reviewer commands. Server composes this router behind access middleware.
use axum::{
    Extension, Json, Router,
    extract::{Path, Query, State},
    http::{StatusCode, header},
    response::IntoResponse,
    routing::{get, post},
};
use litecr_application::{
    access::AccessContext,
    review::{ReviewCaseCursor, ReviewError, ReviewQueueFilter, ReviewService, ReviewStoreError},
};
use litecr_domain::review::{OpenReviewCase, ReviewDecisionCommand, ReviewDecisionKind};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Clone)]
struct ApiState {
    service: Arc<ReviewService>,
}
#[derive(Deserialize)]
struct OpenRequest {
    registry_id: String,
    organization_id: String,
    query_source_record_id: String,
    candidate_source_record_id: String,
    expected_query_receive_version: i64,
    expected_candidate_receive_version: i64,
    #[serde(default)]
    evidence_run_id: Option<String>,
}
#[derive(Deserialize)]
struct QueueQuery {
    registry_id: String,
    organization_id: String,
    #[serde(default = "default_queue_filter")]
    queue_filter: String,
    #[serde(default = "default_limit")]
    limit: u16,
    after_created_at: Option<String>,
    after_case_id: Option<String>,
}
fn default_limit() -> u16 {
    50
}
fn default_queue_filter() -> String {
    "all".into()
}
#[derive(Deserialize)]
struct AssignRequest {
    registry_id: String,
    organization_id: String,
    expected_case_version: i64,
    /// Human OIDC subject within the authenticated assigner's issuer.
    assignee_id: String,
}
#[derive(Deserialize)]
struct DecisionRequest {
    registry_id: String,
    organization_id: String,
    expected_case_version: i64,
    expected_query_receive_version: i64,
    expected_candidate_receive_version: i64,
    expected_query_person_version: i64,
    expected_candidate_person_version: i64,
    decision: String,
    reason_code: String,
    reason: String,
    idempotency_key: String,
    #[serde(default)]
    merge_id: Option<String>,
}
#[derive(Serialize)]
struct Envelope<T> {
    data: T,
    meta: Meta,
}
#[derive(Serialize)]
struct Meta {
    api_version: &'static str,
    replayed: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    has_more: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    next_cursor: Option<QueueCursorData>,
}
#[derive(Serialize)]
struct QueueCursorData {
    queue_filter: String,
    after_created_at: String,
    after_case_id: String,
}
impl From<ReviewCaseCursor> for QueueCursorData {
    fn from(cursor: ReviewCaseCursor) -> Self {
        Self {
            queue_filter: cursor.queue_filter.as_str().into(),
            after_created_at: cursor.created_at,
            after_case_id: cursor.case_id,
        }
    }
}
#[derive(Serialize)]
struct CaseData {
    case_id: String,
    registry_id: String,
    organization_id: String,
    query_source_record_id: String,
    candidate_source_record_id: String,
    evidence_run_id: Option<String>,
    query_receive_version: i64,
    candidate_receive_version: i64,
    query_person_version: i64,
    candidate_person_version: i64,
    case_version: i64,
    state: String,
    assigned_actor_id: Option<String>,
}
impl From<litecr_application::review::ReviewCaseView> for CaseData {
    fn from(c: litecr_application::review::ReviewCaseView) -> Self {
        Self {
            case_id: c.case_id,
            registry_id: c.registry_id,
            organization_id: c.organization_id,
            query_source_record_id: c.query_source_record_id,
            candidate_source_record_id: c.candidate_source_record_id,
            evidence_run_id: c.evidence_run_id,
            query_receive_version: c.query_receive_version,
            candidate_receive_version: c.candidate_receive_version,
            query_person_version: c.query_person_version,
            candidate_person_version: c.candidate_person_version,
            case_version: c.case_version,
            state: c.state,
            assigned_actor_id: c.assigned_actor_id,
        }
    }
}
#[derive(Serialize)]
struct DecisionData {
    case: CaseData,
    decision_id: Option<String>,
    outcome: String,
    merge_id: Option<String>,
}
#[derive(Serialize)]
struct HistoryData {
    decision_id: String,
    decision: String,
    outcome: String,
    reason_code: String,
    reason: String,
    actor_id: String,
    actor_issuer_url: String,
    occurred_at: String,
    merge_id: Option<String>,
}
#[derive(Serialize)]
struct DetailData {
    case: CaseData,
    history: Vec<HistoryData>,
}

pub fn router(service: Arc<ReviewService>) -> Router {
    Router::new()
        .route("/api/v1/review-cases", get(list_cases).post(open_case))
        .route("/api/v1/review-cases/{case_id}", get(detail))
        .route("/api/v1/review-cases/{case_id}/assignment", post(assign))
        .route("/api/v1/review-cases/{case_id}/decisions", post(decide))
        .with_state(ApiState { service })
}
async fn list_cases(
    Extension(ctx): Extension<AccessContext>,
    State(s): State<ApiState>,
    Query(q): Query<QueueQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let queue_filter = match q.queue_filter.as_str() {
        "all" => ReviewQueueFilter::All,
        "active" => ReviewQueueFilter::Active,
        "resolved" => ReviewQueueFilter::Resolved,
        _ => return Err(ApiError::invalid()),
    };
    let after = match (q.after_created_at, q.after_case_id) {
        (None, None) => None,
        (Some(created_at), Some(case_id)) => Some(ReviewCaseCursor {
            queue_filter,
            created_at,
            case_id,
        }),
        _ => return Err(ApiError::invalid()),
    };
    let page = s
        .service
        .list_cases(
            Some(&ctx),
            &q.registry_id,
            &q.organization_id,
            queue_filter,
            q.limit,
            after.as_ref(),
        )
        .await
        .map_err(ApiError::from)?;
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(Envelope {
            data: page
                .cases
                .into_iter()
                .map(CaseData::from)
                .collect::<Vec<_>>(),
            meta: Meta {
                api_version: "v1",
                replayed: None,
                has_more: Some(page.has_more),
                next_cursor: page.next_cursor.map(QueueCursorData::from),
            },
        }),
    ))
}
async fn detail(
    Extension(ctx): Extension<AccessContext>,
    State(s): State<ApiState>,
    Path(id): Path<String>,
    Query(q): Query<QueueQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let detail = s
        .service
        .detail(Some(&ctx), &id, &q.registry_id, &q.organization_id)
        .await
        .map_err(ApiError::from)?;
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(Envelope {
            data: DetailData {
                case: CaseData::from(detail.case),
                history: detail
                    .history
                    .into_iter()
                    .map(|item| HistoryData {
                        decision_id: item.decision_id,
                        decision: item.decision,
                        outcome: item.outcome,
                        reason_code: item.reason_code,
                        reason: item.reason,
                        actor_id: item.actor_id,
                        actor_issuer_url: item.actor_issuer_url,
                        occurred_at: item.occurred_at,
                        merge_id: item.merge_id,
                    })
                    .collect(),
            },
            meta: Meta {
                api_version: "v1",
                replayed: None,
                has_more: None,
                next_cursor: None,
            },
        }),
    ))
}
async fn open_case(
    Extension(ctx): Extension<AccessContext>,
    State(s): State<ApiState>,
    Json(r): Json<OpenRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let c = s
        .service
        .open_case(
            Some(&ctx),
            OpenReviewCase {
                registry_id: r.registry_id,
                organization_id: r.organization_id,
                query_source_record_id: r.query_source_record_id,
                candidate_source_record_id: r.candidate_source_record_id,
                expected_query_receive_version: r.expected_query_receive_version,
                expected_candidate_receive_version: r.expected_candidate_receive_version,
                evidence_run_id: r.evidence_run_id,
            },
        )
        .await
        .map_err(ApiError::from)?;
    Ok((
        StatusCode::CREATED,
        [(header::CACHE_CONTROL, "no-store")],
        Json(Envelope {
            data: CaseData::from(c),
            meta: Meta {
                api_version: "v1",
                replayed: None,
                has_more: None,
                next_cursor: None,
            },
        }),
    ))
}
async fn assign(
    Extension(ctx): Extension<AccessContext>,
    State(s): State<ApiState>,
    Path(id): Path<String>,
    Json(r): Json<AssignRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let c = s
        .service
        .assign(
            Some(&ctx),
            &id,
            &r.registry_id,
            &r.organization_id,
            r.expected_case_version,
            &r.assignee_id,
        )
        .await
        .map_err(ApiError::from)?;
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(Envelope {
            data: CaseData::from(c),
            meta: Meta {
                api_version: "v1",
                replayed: None,
                has_more: None,
                next_cursor: None,
            },
        }),
    ))
}
async fn decide(
    Extension(ctx): Extension<AccessContext>,
    State(s): State<ApiState>,
    Path(id): Path<String>,
    Json(r): Json<DecisionRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let kind = match r.decision.as_str() {
        "confirmed_non_match" => ReviewDecisionKind::ConfirmedNonMatch,
        "link" => ReviewDecisionKind::Link,
        "merge" => ReviewDecisionKind::Merge,
        "reverse_merge" => ReviewDecisionKind::ReverseMerge,
        _ => return Err(ApiError::invalid()),
    };
    let result = s
        .service
        .decide(
            Some(&ctx),
            &r.registry_id,
            &r.organization_id,
            ReviewDecisionCommand {
                case_id: id,
                expected_case_version: r.expected_case_version,
                expected_query_receive_version: r.expected_query_receive_version,
                expected_candidate_receive_version: r.expected_candidate_receive_version,
                expected_query_person_version: r.expected_query_person_version,
                expected_candidate_person_version: r.expected_candidate_person_version,
                kind,
                reason_code: r.reason_code,
                reason: r.reason,
                idempotency_key: r.idempotency_key,
                merge_id: r.merge_id,
            },
        )
        .await
        .map_err(ApiError::from)?;
    Ok((
        StatusCode::OK,
        [(header::CACHE_CONTROL, "no-store")],
        Json(Envelope {
            data: DecisionData {
                case: CaseData::from(result.case),
                decision_id: result.decision_id,
                outcome: result.outcome,
                merge_id: result.merge_id,
            },
            meta: Meta {
                api_version: "v1",
                replayed: Some(result.replayed),
                has_more: None,
                next_cursor: None,
            },
        }),
    ))
}

struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: &'static str,
    retryable: bool,
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
impl ApiError {
    fn invalid() -> Self {
        Self {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            code: "INVALID_REVIEW_COMMAND",
            message: "The review request is invalid.",
            retryable: false,
        }
    }
}
impl From<ReviewError> for ApiError {
    fn from(e: ReviewError) -> Self {
        match e {
            ReviewError::AccessDenied(_) => Self {
                status: StatusCode::FORBIDDEN,
                code: "ACCESS_DENIED",
                message: "The requested operation is not permitted.",
                retryable: false,
            },
            ReviewError::InvalidCommand => Self::invalid(),
            ReviewError::Store(ReviewStoreError::NotFound) => Self {
                status: StatusCode::NOT_FOUND,
                code: "REVIEW_CASE_NOT_FOUND",
                message: "The review case was not found.",
                retryable: false,
            },
            ReviewError::Store(ReviewStoreError::Conflict) => Self {
                status: StatusCode::CONFLICT,
                code: "REVIEW_CONFLICT",
                message: "The review state changed; refresh before deciding.",
                retryable: false,
            },
            ReviewError::Store(ReviewStoreError::EvidenceMismatch) => Self {
                status: StatusCode::UNPROCESSABLE_ENTITY,
                code: "INVALID_REVIEW_EVIDENCE",
                message: "The matching evidence does not belong to this source pair and version.",
                retryable: false,
            },
            ReviewError::Store(ReviewStoreError::RequiresReview) => Self {
                status: StatusCode::CONFLICT,
                code: "REVERSAL_REQUIRES_REVIEW",
                message: "Later changes depend on this merge; request a new review before reversing it.",
                retryable: false,
            },
            ReviewError::Store(ReviewStoreError::Unavailable) => Self {
                status: StatusCode::SERVICE_UNAVAILABLE,
                code: "REVIEW_UNAVAILABLE",
                message: "The review service is temporarily unavailable.",
                retryable: true,
            },
        }
    }
}
impl IntoResponse for ApiError {
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

    #[test]
    fn mismatched_evidence_has_a_non_retryable_client_error() {
        let error = ApiError::from(ReviewError::Store(ReviewStoreError::EvidenceMismatch));
        assert_eq!(error.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(error.code, "INVALID_REVIEW_EVIDENCE");
        assert!(!error.retryable);
    }

    #[test]
    fn review_case_json_exposes_optional_evidence_run_id() {
        let case = CaseData {
            case_id: "case-synthetic".into(),
            registry_id: "reg-synthetic".into(),
            organization_id: "org-synthetic".into(),
            query_source_record_id: "query-synthetic".into(),
            candidate_source_record_id: "candidate-synthetic".into(),
            evidence_run_id: Some("abcdefab-cdef-abcd-efab-cdefabcdefab".into()),
            query_receive_version: 1,
            candidate_receive_version: 2,
            query_person_version: 1,
            candidate_person_version: 1,
            case_version: 1,
            state: "open".into(),
            assigned_actor_id: None,
        };
        let encoded = serde_json::to_value(case).expect("case serializes");
        assert_eq!(
            encoded["evidence_run_id"],
            "abcdefab-cdef-abcd-efab-cdefabcdefab"
        );
    }

    #[test]
    fn legacy_open_case_request_without_evidence_remains_accepted() {
        let request: OpenRequest = serde_json::from_value(serde_json::json!({
            "registry_id": "reg-synthetic",
            "organization_id": "org-synthetic",
            "query_source_record_id": "query-synthetic",
            "candidate_source_record_id": "candidate-synthetic",
            "expected_query_receive_version": 1,
            "expected_candidate_receive_version": 1
        }))
        .expect("legacy request shape remains decodable");
        assert!(request.evidence_run_id.is_none());
    }

    #[test]
    fn review_queue_metadata_returns_an_explicit_keyset_continuation() {
        let meta = Meta {
            api_version: "v1",
            replayed: None,
            has_more: Some(true),
            next_cursor: Some(QueueCursorData::from(ReviewCaseCursor {
                queue_filter: ReviewQueueFilter::Active,
                created_at: "2026-10-09T12:34:56.123456Z".into(),
                case_id: "123e4567-e89b-12d3-a456-426614174000".into(),
            })),
        };
        let value = serde_json::to_value(meta).expect("queue metadata serializes");
        assert_eq!(value["has_more"], true);
        assert_eq!(value["next_cursor"]["queue_filter"], "active");
        assert_eq!(
            value["next_cursor"]["after_created_at"],
            "2026-10-09T12:34:56.123456Z"
        );
        assert_eq!(
            value["next_cursor"]["after_case_id"],
            "123e4567-e89b-12d3-a456-426614174000"
        );
    }
}
