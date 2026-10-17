//! Protected registry HTTP routes. The server composes this router behind access middleware.

use std::sync::Arc;

use axum::{
    Extension, Json, Router,
    extract::Path,
    http::{HeaderMap, StatusCode, header},
    response::IntoResponse,
    routing::{get, post},
};
use litecr_application::{
    access::AccessContext,
    registry::{
        IdentifierAssertion, IngestionOutcome, PersonView, RegistryError, RegistryService,
        SourceAssertion, SourceAssertionView, SourceRecordCommand,
    },
};
use serde::{Deserialize, Serialize};

#[derive(Clone)]
struct ApiState {
    service: Arc<RegistryService>,
}

#[derive(Debug, Deserialize)]
struct SourceRecordUpsertRequest {
    registry_id: String,
    organization_id: String,
    source_system_id: String,
    source_record_key: String,
    #[serde(default)]
    source_version: Option<String>,
    assertions: Vec<SourceAssertionRequest>,
    #[serde(default)]
    identifiers: Vec<IdentifierAssertionRequest>,
}

#[derive(Debug, Deserialize)]
struct SourceAssertionRequest {
    field_path: String,
    original_value: String,
    #[serde(default)]
    language_tag: Option<String>,
    #[serde(default)]
    script: Option<String>,
}

#[derive(Debug, Deserialize)]
struct IdentifierAssertionRequest {
    authority: String,
    identifier_type: String,
    value: String,
    #[serde(default)]
    authoritative: bool,
}

#[derive(Debug, Deserialize)]
struct PersonSearchRequest {
    registry_id: String,
    organization_id: String,
    public_id_or_alias: String,
    #[serde(default = "default_limit")]
    limit: u16,
}

fn default_limit() -> u16 {
    20
}

#[derive(Debug, Serialize)]
struct ApiEnvelope<T> {
    data: T,
    meta: ApiMeta,
}

#[derive(Debug, Serialize)]
struct ApiMeta {
    api_version: &'static str,
    replayed: Option<bool>,
}

#[derive(Debug, Serialize)]
struct IngestionResponse {
    outcome: &'static str,
    receipt_id: String,
    source_record_id: String,
    source_receive_version: i64,
    person_public_id: String,
}

#[derive(Debug, Serialize)]
struct PersonResponse {
    public_id: String,
    resource_version: i64,
    source_record_ids: Vec<String>,
    assertions: Vec<SourceAssertionResponse>,
    aliases: Vec<String>,
}

#[derive(Debug, Serialize)]
struct SourceAssertionResponse {
    source_record_id: String,
    source_system_id: String,
    source_version: Option<String>,
    receive_version: i64,
    field_path: String,
    original_value: String,
    language_tag: Option<String>,
    script: Option<String>,
}

impl From<PersonView> for PersonResponse {
    fn from(person: PersonView) -> Self {
        Self {
            public_id: person.public_id,
            resource_version: person.resource_version,
            source_record_ids: person.source_record_ids,
            assertions: person.assertions.into_iter().map(Into::into).collect(),
            aliases: person.aliases,
        }
    }
}

impl From<SourceAssertionView> for SourceAssertionResponse {
    fn from(assertion: SourceAssertionView) -> Self {
        Self {
            source_record_id: assertion.source_record_id,
            source_system_id: assertion.source_system_id,
            source_version: assertion.source_version,
            receive_version: assertion.receive_version,
            field_path: assertion.field_path,
            original_value: assertion.original_value,
            language_tag: assertion.language_tag,
            script: assertion.script,
        }
    }
}

#[derive(Debug, Serialize)]
struct ApiErrorBody {
    error: ApiError,
}

#[derive(Debug, Serialize)]
struct ApiError {
    code: &'static str,
    message: &'static str,
    retryable: bool,
}

pub fn router(service: Arc<RegistryService>) -> Router {
    Router::new()
        .route("/api/v1/source-records", post(upsert_source_record))
        .route("/api/v1/person-search", post(search_person))
        .route(
            "/api/v1/registries/{registry_id}/organizations/{organization_id}/source-records/{source_record_id}",
            get(source_record_person),
        )
        .route(
            "/api/v1/registries/{registry_id}/organizations/{organization_id}/persons/{public_id}",
            get(person_detail),
        )
        .with_state(ApiState { service })
}

async fn upsert_source_record(
    Extension(context): Extension<AccessContext>,
    axum::extract::State(state): axum::extract::State<ApiState>,
    headers: HeaderMap,
    Json(request): Json<SourceRecordUpsertRequest>,
) -> Result<impl IntoResponse, ApiResponseError> {
    let idempotency_key = headers
        .get("idempotency-key")
        .and_then(|value| value.to_str().ok())
        .ok_or(ApiResponseError::invalid("IDEMPOTENCY_KEY_REQUIRED"))?;
    let command = SourceRecordCommand {
        registry_id: request.registry_id,
        organization_id: request.organization_id,
        source_system_id: request.source_system_id,
        source_record_key: request.source_record_key,
        source_version: request.source_version,
        assertions: request
            .assertions
            .into_iter()
            .map(|item| SourceAssertion {
                field_path: item.field_path,
                original_value: item.original_value,
                language_tag: item.language_tag,
                script: item.script,
            })
            .collect(),
        identifiers: request
            .identifiers
            .into_iter()
            .map(|item| IdentifierAssertion {
                authority: item.authority,
                identifier_type: item.identifier_type,
                value: item.value,
                authoritative: item.authoritative,
            })
            .collect(),
    };
    let receipt = state
        .service
        .ingest(Some(&context), idempotency_key, command)
        .await
        .map_err(ApiResponseError::from_registry)?;
    let outcome = match receipt.outcome {
        IngestionOutcome::Accepted => "accepted",
        IngestionOutcome::SourceConflict => "source_conflict",
    };
    let status = if receipt.outcome == IngestionOutcome::SourceConflict {
        StatusCode::CONFLICT
    } else if receipt.replayed {
        StatusCode::OK
    } else {
        StatusCode::ACCEPTED
    };
    Ok((
        status,
        Json(ApiEnvelope {
            data: IngestionResponse {
                outcome,
                receipt_id: receipt.source_record_id.clone(),
                source_record_id: receipt.source_record_id,
                source_receive_version: receipt.source_receive_version,
                person_public_id: receipt.person_public_id,
            },
            meta: ApiMeta {
                api_version: "v1",
                replayed: Some(receipt.replayed),
            },
        }),
    ))
}

async fn search_person(
    Extension(context): Extension<AccessContext>,
    axum::extract::State(state): axum::extract::State<ApiState>,
    Json(request): Json<PersonSearchRequest>,
) -> Result<Json<ApiEnvelope<Vec<PersonResponse>>>, ApiResponseError> {
    let people = state
        .service
        .search_public_id(
            Some(&context),
            &request.registry_id,
            &request.organization_id,
            &request.public_id_or_alias,
            request.limit,
        )
        .await
        .map_err(ApiResponseError::from_registry)?;
    Ok(Json(ApiEnvelope {
        data: people.into_iter().map(Into::into).collect(),
        meta: ApiMeta {
            api_version: "v1",
            replayed: None,
        },
    }))
}

async fn person_detail(
    Extension(context): Extension<AccessContext>,
    axum::extract::State(state): axum::extract::State<ApiState>,
    Path((registry_id, organization_id, public_id)): Path<(String, String, String)>,
) -> Result<Json<ApiEnvelope<PersonResponse>>, ApiResponseError> {
    let person = state
        .service
        .person(Some(&context), &registry_id, &organization_id, &public_id)
        .await
        .map_err(ApiResponseError::from_registry)?
        .ok_or(ApiResponseError {
            status: StatusCode::NOT_FOUND,
            code: "PERSON_NOT_FOUND",
            message: "The requested person was not found.",
            retryable: false,
        })?;
    Ok(Json(ApiEnvelope {
        data: person.into(),
        meta: ApiMeta {
            api_version: "v1",
            replayed: None,
        },
    }))
}

async fn source_record_person(
    Extension(context): Extension<AccessContext>,
    axum::extract::State(state): axum::extract::State<ApiState>,
    Path((registry_id, organization_id, source_record_id)): Path<(String, String, String)>,
) -> Result<Json<ApiEnvelope<PersonResponse>>, ApiResponseError> {
    let person = state
        .service
        .source_record_person(
            Some(&context),
            &registry_id,
            &organization_id,
            &source_record_id,
        )
        .await
        .map_err(ApiResponseError::from_registry)?
        .ok_or(ApiResponseError {
            status: StatusCode::NOT_FOUND,
            code: "SOURCE_RECORD_NOT_FOUND",
            message: "The requested source record was not found.",
            retryable: false,
        })?;
    Ok(Json(ApiEnvelope {
        data: person.into(),
        meta: ApiMeta {
            api_version: "v1",
            replayed: None,
        },
    }))
}

struct ApiResponseError {
    status: StatusCode,
    code: &'static str,
    message: &'static str,
    retryable: bool,
}

impl ApiResponseError {
    fn invalid(code: &'static str) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code,
            message: "The request is invalid.",
            retryable: false,
        }
    }

    fn from_registry(error: RegistryError) -> Self {
        match error {
            RegistryError::AccessDenied(_) => Self {
                status: StatusCode::FORBIDDEN,
                code: "ACCESS_DENIED",
                message: "The requested operation is not permitted.",
                retryable: false,
            },
            RegistryError::InvalidCommand(_) | RegistryError::InvalidIdempotencyKey => Self {
                status: StatusCode::UNPROCESSABLE_ENTITY,
                code: "INVALID_REGISTRY_COMMAND",
                message: "The registry request is invalid.",
                retryable: false,
            },
            RegistryError::Store(
                litecr_application::registry::RegistryStoreError::IdempotencyKeyReused,
            ) => Self {
                status: StatusCode::CONFLICT,
                code: "IDEMPOTENCY_KEY_REUSED",
                message: "The idempotency key was used for a different command.",
                retryable: false,
            },
            RegistryError::Store(litecr_application::registry::RegistryStoreError::Unavailable) => {
                Self {
                    status: StatusCode::SERVICE_UNAVAILABLE,
                    code: "REGISTRY_UNAVAILABLE",
                    message: "The registry is temporarily unavailable.",
                    retryable: true,
                }
            }
        }
    }
}

impl IntoResponse for ApiResponseError {
    fn into_response(self) -> axum::response::Response {
        (
            self.status,
            [(header::CACHE_CONTROL, "no-store")],
            Json(ApiErrorBody {
                error: ApiError {
                    code: self.code,
                    message: self.message,
                    retryable: self.retryable,
                },
            }),
        )
            .into_response()
    }
}
