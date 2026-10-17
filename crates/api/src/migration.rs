//! Protected operator endpoints for applying and withdrawing staged migration batches.
//!
//! The router is deliberately mounted behind `protected_routes`. Identity and
//! grants come from the middleware-created `AccessContext`; request data never
//! supplies an actor or widens its scope.

use std::sync::Arc;

use axum::{
    Extension, Json, Router,
    body::Bytes,
    extract::{DefaultBodyLimit, rejection::JsonRejection},
    http::{HeaderValue, StatusCode, header},
    response::IntoResponse,
    routing::{get, post, put},
};
use litecr_application::{
    access::AccessContext,
    migration::{
        MigrationBatch, MigrationError, MigrationPackagePolicy, MigrationPackageV2Receipt,
        MigrationRecord, MigrationRun, MigrationRunState, MigrationService, QuarantineCorrection,
        QuarantineDisposition, QuarantineDispositionAction, ReverseBlockerCount, ReverseManifest,
        ReversePreflight, ReversePreflightRequest,
    },
};
use serde::{Deserialize, Serialize};

/// Keep transport bodies bounded even though application batches allow 64 MiB
/// of raw NDJSON. JSON string escaping can expand that payload, so the API
/// advertises an 8 MiB raw-batch limit and permits at most 50 MiB on the wire.
pub const MAX_API_BATCH_BYTES: usize = 8 * 1024 * 1024;
const MAX_JSON_BODY_BYTES: usize = 50 * 1024 * 1024;
const MAX_DIGEST_HEX_LEN: usize = 64;

#[derive(Clone)]
struct ApiState {
    service: Arc<MigrationService>,
    package_v2_policy: Option<MigrationPackagePolicy>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RunRequest {
    manifest_sha256: String,
    stream_sha256: String,
    snapshot_id: String,
    source_system_id: String,
    registry_id: String,
    organization_id: String,
    expected_record_count: u64,
}

impl TryFrom<RunRequest> for MigrationRun {
    type Error = ApiError;

    fn try_from(value: RunRequest) -> Result<Self, Self::Error> {
        validate_identifier(&value.snapshot_id, 256)?;
        validate_identifier(&value.source_system_id, 128)?;
        validate_identifier(&value.registry_id, 128)?;
        validate_identifier(&value.organization_id, 128)?;
        if value.expected_record_count > i64::MAX as u64 {
            return Err(ApiError::invalid());
        }
        Ok(Self {
            manifest_sha256: decode_digest(&value.manifest_sha256)?,
            stream_sha256: decode_digest(&value.stream_sha256)?,
            snapshot_id: value.snapshot_id,
            source_system_id: value.source_system_id,
            registry_id: value.registry_id,
            organization_id: value.organization_id,
            expected_record_count: value.expected_record_count,
        })
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RecordRequest {
    source_system_id: String,
    source_record_key: String,
    source_version: String,
    payload_sha256: String,
    /// Exact UTF-8 source row including its terminating LF.
    raw_ndjson: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BatchRequest {
    run: RunRequest,
    sequence: u64,
    first_input_line: u64,
    last_input_line: u64,
    batch_sha256: String,
    records: Vec<RecordRequest>,
}

impl TryFrom<BatchRequest> for MigrationBatch {
    type Error = ApiError;

    fn try_from(value: BatchRequest) -> Result<Self, Self::Error> {
        let run = MigrationRun::try_from(value.run)?;
        if value.records.is_empty() || value.records.len() > 500 {
            return Err(ApiError::invalid());
        }
        let mut exact_bytes = Vec::new();
        let mut records = Vec::with_capacity(value.records.len());
        for record in value.records {
            validate_identifier(&record.source_system_id, 128)?;
            validate_identifier(&record.source_record_key, 512)?;
            validate_identifier(&record.source_version, 256)?;
            let raw_ndjson = record.raw_ndjson.into_bytes();
            if raw_ndjson.is_empty()
                || raw_ndjson.len() > litecr_application::migration::MAX_RECORD_BYTES
                || !raw_ndjson.ends_with(b"\n")
            {
                return Err(ApiError::invalid());
            }
            exact_bytes.extend_from_slice(&raw_ndjson);
            if exact_bytes.len() > MAX_API_BATCH_BYTES {
                return Err(ApiError::too_large());
            }
            records.push(MigrationRecord {
                source_system_id: record.source_system_id,
                source_record_key: record.source_record_key,
                source_version: record.source_version,
                payload_sha256: decode_digest(&record.payload_sha256)?,
                raw_ndjson,
            });
        }
        Ok(Self {
            run,
            sequence: value.sequence,
            first_input_line: value.first_input_line,
            last_input_line: value.last_input_line,
            batch_sha256: decode_digest(&value.batch_sha256)?,
            exact_bytes,
            records,
        })
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CheckpointRequest {
    run: RunRequest,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReverseRequest {
    manifest_sha256: String,
    /// The caller must repeat the digest explicitly to authorize this write.
    confirm_manifest_sha256: String,
    registry_id: String,
    organization_id: String,
    reason: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReversePreflightBody {
    manifest_sha256: String,
    registry_id: String,
    organization_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct QuarantineDispositionRequest {
    action: String,
    parent_manifest_sha256: String,
    replacement_manifest_sha256: Option<String>,
    registry_id: String,
    organization_id: String,
    reason: String,
    idempotency_key: String,
    corrections: Vec<QuarantineCorrectionRequest>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct QuarantineCorrectionRequest {
    input_line: u64,
    duplicate_of_line: Option<u64>,
    quarantine_code: String,
    original_payload_sha256: String,
    corrected_payload_sha256: Option<String>,
}

#[derive(Debug, Serialize)]
struct Envelope<T> {
    data: T,
    meta: Meta,
}

#[derive(Debug, Serialize)]
struct Meta {
    api_version: &'static str,
}

#[derive(Debug, Serialize)]
struct CheckpointData {
    manifest_sha256: String,
    committed_batch_sequence: u64,
    committed_record_count: u64,
    state: &'static str,
}

#[derive(Debug, Serialize)]
struct ReverseData {
    manifest_sha256: String,
    withdrawn_record_count: u64,
    detached_identifier_count: u64,
    retained_alias_count: u64,
    state: &'static str,
}

#[derive(Debug, Serialize)]
struct ReversePreflightData {
    eligible: bool,
    blockers: Vec<ReverseBlockerData>,
}

#[derive(Debug, Serialize)]
struct QuarantineDispositionData {
    disposition_id: String,
    action: &'static str,
    replacement_manifest_sha256: Option<String>,
    correction_count: u64,
    replayed: bool,
}

#[derive(Debug, Serialize)]
struct ReverseBlockerData {
    code: &'static str,
    count: u64,
}

#[derive(Debug, Serialize)]
struct ErrorEnvelope {
    error: ErrorBody,
}

#[derive(Debug, Serialize)]
struct ErrorBody {
    code: &'static str,
    message: &'static str,
    retryable: bool,
}

/// Routes are mounted behind the application's existing access middleware.
/// Every mutating operation also passes the context to `MigrationService`,
/// which enforces `MigrationWrite` on the exact registry/organization pair.
pub fn router(service: Arc<MigrationService>) -> Router {
    router_with_package_v2(service, None)
}

/// Mount the separate exact-byte package v2 routes only when every deployment
/// storage policy setting has been supplied and validated by the host.
pub fn router_with_package_v2(
    service: Arc<MigrationService>,
    package_v2_policy: Option<MigrationPackagePolicy>,
) -> Router {
    let mut routes = Router::new()
        .route("/api/v1/migrations/batches", post(commit_batch))
        .route("/api/v1/migrations/batches/validate", post(validate_batch))
        .route("/api/v1/migrations/checkpoint", post(checkpoint))
        .route(
            "/api/v1/migrations/quarantine-dispositions",
            post(record_quarantine_disposition),
        )
        .route("/api/v1/migrations/reverse", post(reverse))
        .route(
            "/api/v1/migrations/reverse/preflight",
            post(reverse_preflight),
        );
    if package_v2_policy.is_some() {
        let package_routes = Router::new()
            .route("/api/v2/migration-packages", post(create_package_v2))
            .route(
                "/api/v2/migration-packages/{upload_id}",
                get(inspect_package_v2),
            )
            .route(
                "/api/v2/migration-packages/{upload_id}/batches/{sequence}",
                put(stage_package_v2_batch),
            )
            .route(
                "/api/v2/migration-packages/{upload_id}/finalize",
                post(finalize_package_v2),
            )
            .route(
                "/api/v2/migration-packages/{upload_id}/apply",
                post(apply_package_v2_batch),
            )
            .layer(DefaultBodyLimit::max(
                litecr_application::migration::MAX_API_BATCH_BYTES,
            ));
        routes = routes.merge(package_routes);
    }
    routes
        .layer(DefaultBodyLimit::max(MAX_JSON_BODY_BYTES))
        .with_state(ApiState {
            service,
            package_v2_policy,
        })
}

/*
 * v1 remains frozen above. v2 accepts exact JSON manifest bytes and raw
 * application/x-ndjson batch bytes, never re-encoded JSON row strings.
 */
#[derive(Debug, Serialize)]
struct PackageV2Data {
    upload_id: String,
    manifest_sha256: String,
    stream_sha256: String,
    state: String,
    staged_batch_count: u64,
    staged_bytes: u64,
    record_count: u64,
    committed_batch_sequence: u64,
    committed_record_count: u64,
}

impl From<MigrationPackageV2Receipt> for PackageV2Data {
    fn from(value: MigrationPackageV2Receipt) -> Self {
        Self {
            upload_id: value.upload_id,
            manifest_sha256: encode_digest(&value.manifest_sha256),
            stream_sha256: encode_digest(&value.stream_sha256),
            state: value.state,
            staged_batch_count: value.staged_batch_count,
            staged_bytes: value.staged_bytes,
            record_count: value.record_count,
            committed_batch_sequence: value.committed_batch_sequence,
            committed_record_count: value.committed_record_count,
        }
    }
}

async fn create_package_v2(
    Extension(context): Extension<AccessContext>,
    axum::extract::State(state): axum::extract::State<ApiState>,
    body: Bytes,
) -> Result<impl IntoResponse, ApiError> {
    let policy = state
        .package_v2_policy
        .as_ref()
        .ok_or_else(ApiError::v2_disabled)?;
    if body.len() > litecr_application::migration::package::MAX_MANIFEST_BYTES {
        return Err(ApiError::too_large());
    }
    let result = state
        .service
        .create_package_v2(Some(&context), &body, policy)
        .await
        .map_err(ApiError::from_migration)?;
    Ok((
        StatusCode::CREATED,
        no_store(),
        Json(Envelope {
            data: PackageV2Data::from(result),
            meta: Meta { api_version: "v2" },
        }),
    ))
}

async fn stage_package_v2_batch(
    Extension(context): Extension<AccessContext>,
    axum::extract::State(state): axum::extract::State<ApiState>,
    axum::extract::Path((upload_id, sequence)): axum::extract::Path<(String, u64)>,
    body: Bytes,
) -> Result<impl IntoResponse, ApiError> {
    let policy = state
        .package_v2_policy
        .as_ref()
        .ok_or_else(ApiError::v2_disabled)?;
    if body.len() > litecr_application::migration::MAX_API_BATCH_BYTES {
        return Err(ApiError::too_large());
    }
    let result = state
        .service
        .stage_package_v2_batch(Some(&context), &upload_id, sequence, &body, policy)
        .await
        .map_err(ApiError::from_migration)?;
    Ok((
        StatusCode::ACCEPTED,
        no_store(),
        Json(Envelope {
            data: PackageV2Data::from(result),
            meta: Meta { api_version: "v2" },
        }),
    ))
}

async fn finalize_package_v2(
    Extension(context): Extension<AccessContext>,
    axum::extract::State(state): axum::extract::State<ApiState>,
    axum::extract::Path(upload_id): axum::extract::Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let policy = state
        .package_v2_policy
        .as_ref()
        .ok_or_else(ApiError::v2_disabled)?;
    let result = state
        .service
        .finalize_package_v2(Some(&context), &upload_id, policy)
        .await
        .map_err(ApiError::from_migration)?;
    Ok((
        StatusCode::OK,
        no_store(),
        Json(Envelope {
            data: PackageV2Data::from(result),
            meta: Meta { api_version: "v2" },
        }),
    ))
}

async fn inspect_package_v2(
    Extension(context): Extension<AccessContext>,
    axum::extract::State(state): axum::extract::State<ApiState>,
    axum::extract::Path(upload_id): axum::extract::Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let result = state
        .service
        .inspect_package_v2(Some(&context), &upload_id)
        .await
        .map_err(ApiError::from_migration)?;
    Ok((
        StatusCode::OK,
        no_store(),
        Json(Envelope {
            data: PackageV2Data::from(result),
            meta: Meta { api_version: "v2" },
        }),
    ))
}

async fn apply_package_v2_batch(
    Extension(context): Extension<AccessContext>,
    axum::extract::State(state): axum::extract::State<ApiState>,
    axum::extract::Path(upload_id): axum::extract::Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let result = state
        .service
        .apply_package_v2_batch(Some(&context), &upload_id)
        .await
        .map_err(ApiError::from_migration)?;
    Ok((
        StatusCode::ACCEPTED,
        no_store(),
        Json(Envelope {
            data: PackageV2Data::from(result),
            meta: Meta { api_version: "v2" },
        }),
    ))
}

async fn record_quarantine_disposition(
    Extension(context): Extension<AccessContext>,
    axum::extract::State(state): axum::extract::State<ApiState>,
    body: Result<Json<QuarantineDispositionRequest>, JsonRejection>,
) -> Result<impl IntoResponse, ApiError> {
    let Json(request) = body.map_err(ApiError::from_json)?;
    validate_identifier(&request.registry_id, 128)?;
    validate_identifier(&request.organization_id, 128)?;
    if request.corrections.len() > 1000 {
        return Err(ApiError::invalid());
    }
    let action = match request.action.as_str() {
        "corrected_resubmission" => QuarantineDispositionAction::CorrectedResubmission,
        "deferred" => QuarantineDispositionAction::Deferred,
        "duplicate_acknowledged" => QuarantineDispositionAction::DuplicateAcknowledged,
        _ => return Err(ApiError::invalid()),
    };
    let corrections = request
        .corrections
        .into_iter()
        .map(|item| {
            Ok(QuarantineCorrection {
                input_line: item.input_line,
                duplicate_of_line: item.duplicate_of_line,
                quarantine_code: item.quarantine_code,
                original_payload_sha256: decode_digest(&item.original_payload_sha256)?,
                corrected_payload_sha256: item
                    .corrected_payload_sha256
                    .as_deref()
                    .map(decode_digest)
                    .transpose()?,
            })
        })
        .collect::<Result<Vec<_>, ApiError>>()?;
    let receipt = state
        .service
        .record_quarantine_disposition(
            Some(&context),
            QuarantineDisposition {
                action,
                parent_manifest_sha256: decode_digest(&request.parent_manifest_sha256)?,
                replacement_manifest_sha256: request
                    .replacement_manifest_sha256
                    .as_deref()
                    .map(decode_digest)
                    .transpose()?,
                registry_id: request.registry_id,
                organization_id: request.organization_id,
                reason: request.reason,
                idempotency_key: request.idempotency_key,
                corrections,
            },
        )
        .await
        .map_err(ApiError::from_migration)?;
    Ok((
        StatusCode::OK,
        no_store(),
        Json(Envelope {
            data: QuarantineDispositionData {
                disposition_id: receipt.disposition_id,
                action: disposition_action(receipt.action),
                replacement_manifest_sha256: receipt
                    .replacement_manifest_sha256
                    .as_ref()
                    .map(encode_digest),
                correction_count: receipt.correction_count,
                replayed: receipt.replayed,
            },
            meta: Meta { api_version: "v1" },
        }),
    ))
}

fn disposition_action(action: QuarantineDispositionAction) -> &'static str {
    match action {
        QuarantineDispositionAction::CorrectedResubmission => "corrected_resubmission",
        QuarantineDispositionAction::Deferred => "deferred",
        QuarantineDispositionAction::DuplicateAcknowledged => "duplicate_acknowledged",
    }
}

async fn commit_batch(
    Extension(context): Extension<AccessContext>,
    axum::extract::State(state): axum::extract::State<ApiState>,
    body: Result<Json<BatchRequest>, JsonRejection>,
) -> Result<impl IntoResponse, ApiError> {
    let Json(request) = body.map_err(ApiError::from_json)?;
    let batch = MigrationBatch::try_from(request)?;
    let digest = encode_digest(&batch.run.manifest_sha256);
    let result = state
        .service
        .commit_batch(Some(&context), batch)
        .await
        .map_err(ApiError::from_migration)?;
    Ok((
        StatusCode::ACCEPTED,
        no_store(),
        Json(Envelope {
            data: CheckpointData {
                manifest_sha256: digest,
                committed_batch_sequence: result.checkpoint.committed_batch_sequence,
                committed_record_count: result.checkpoint.committed_record_count,
                state: run_state(result.checkpoint.state),
            },
            meta: Meta { api_version: "v1" },
        }),
    ))
}

async fn validate_batch(
    Extension(context): Extension<AccessContext>,
    axum::extract::State(state): axum::extract::State<ApiState>,
    body: Result<Json<BatchRequest>, JsonRejection>,
) -> Result<impl IntoResponse, ApiError> {
    let Json(request) = body.map_err(ApiError::from_json)?;
    let batch = MigrationBatch::try_from(request)?;
    state
        .service
        .validate_batch(Some(&context), &batch)
        .await
        .map_err(ApiError::from_migration)?;
    Ok((StatusCode::NO_CONTENT, no_store()))
}

async fn checkpoint(
    Extension(context): Extension<AccessContext>,
    axum::extract::State(state): axum::extract::State<ApiState>,
    body: Result<Json<CheckpointRequest>, JsonRejection>,
) -> Result<impl IntoResponse, ApiError> {
    let Json(request) = body.map_err(ApiError::from_json)?;
    let run = MigrationRun::try_from(request.run)?;
    let digest = encode_digest(&run.manifest_sha256);
    let checkpoint = state
        .service
        .checkpoint(Some(&context), &run)
        .await
        .map_err(ApiError::from_migration)?;
    let Some(checkpoint) = checkpoint else {
        return Err(ApiError {
            status: StatusCode::NOT_FOUND,
            code: "MIGRATION_NOT_FOUND",
            message: "The migration checkpoint was not found.",
            retryable: false,
        });
    };
    Ok((
        StatusCode::OK,
        no_store(),
        Json(Envelope {
            data: CheckpointData {
                manifest_sha256: digest,
                committed_batch_sequence: checkpoint.committed_batch_sequence,
                committed_record_count: checkpoint.committed_record_count,
                state: run_state(checkpoint.state),
            },
            meta: Meta { api_version: "v1" },
        }),
    ))
}

async fn reverse(
    Extension(context): Extension<AccessContext>,
    axum::extract::State(state): axum::extract::State<ApiState>,
    body: Result<Json<ReverseRequest>, JsonRejection>,
) -> Result<impl IntoResponse, ApiError> {
    let Json(request) = body.map_err(ApiError::from_json)?;
    validate_identifier(&request.registry_id, 128)?;
    validate_identifier(&request.organization_id, 128)?;
    if request.reason.trim().is_empty()
        || request.reason.len() > 2000
        || request.reason.chars().any(char::is_control)
    {
        return Err(ApiError::invalid());
    }
    let manifest_sha256 = decode_digest(&request.manifest_sha256)?;
    let confirmed = decode_digest(&request.confirm_manifest_sha256)?;
    if manifest_sha256 != confirmed {
        return Err(ApiError::invalid());
    }
    let digest = encode_digest(&manifest_sha256);
    let receipt = state
        .service
        .reverse_manifest(
            Some(&context),
            ReverseManifest {
                manifest_sha256,
                registry_id: request.registry_id,
                organization_id: request.organization_id,
                reason: request.reason,
            },
        )
        .await
        .map_err(ApiError::from_migration)?;
    Ok((
        StatusCode::OK,
        no_store(),
        Json(Envelope {
            data: ReverseData {
                manifest_sha256: digest,
                withdrawn_record_count: receipt.withdrawn_record_count,
                detached_identifier_count: receipt.detached_identifier_count,
                retained_alias_count: receipt.retained_alias_count,
                state: run_state(receipt.state),
            },
            meta: Meta { api_version: "v1" },
        }),
    ))
}

async fn reverse_preflight(
    Extension(context): Extension<AccessContext>,
    axum::extract::State(state): axum::extract::State<ApiState>,
    body: Result<Json<ReversePreflightBody>, JsonRejection>,
) -> Result<impl IntoResponse, ApiError> {
    let Json(request) = body.map_err(ApiError::from_json)?;
    validate_identifier(&request.registry_id, 128)?;
    validate_identifier(&request.organization_id, 128)?;
    let result = state
        .service
        .reverse_preflight(
            Some(&context),
            &ReversePreflightRequest {
                manifest_sha256: decode_digest(&request.manifest_sha256)?,
                registry_id: request.registry_id,
                organization_id: request.organization_id,
            },
        )
        .await
        .map_err(ApiError::from_migration)?;
    Ok((
        StatusCode::OK,
        no_store(),
        Json(Envelope {
            data: reverse_preflight_data(result),
            meta: Meta { api_version: "v1" },
        }),
    ))
}

fn reverse_preflight_data(result: ReversePreflight) -> ReversePreflightData {
    let mut blockers = result.blockers;
    blockers.sort_by_key(|blocker| blocker.code.as_str());
    ReversePreflightData {
        eligible: result.eligible,
        blockers: blockers
            .into_iter()
            .map(|ReverseBlockerCount { code, count }| ReverseBlockerData {
                code: code.as_str(),
                count,
            })
            .collect(),
    }
}

fn no_store() -> [(header::HeaderName, HeaderValue); 1] {
    [(header::CACHE_CONTROL, HeaderValue::from_static("no-store"))]
}

fn run_state(state: MigrationRunState) -> &'static str {
    match state {
        MigrationRunState::Validated => "validated",
        MigrationRunState::Importing => "importing",
        MigrationRunState::Completed => "completed",
        MigrationRunState::ReverseBlocked => "reverse_blocked",
        MigrationRunState::Reversed => "reversed",
    }
}

fn validate_identifier(value: &str, max_bytes: usize) -> Result<(), ApiError> {
    if value.trim().is_empty() || value.len() > max_bytes || value.chars().any(char::is_control) {
        return Err(ApiError::invalid());
    }
    Ok(())
}

fn decode_digest(value: &str) -> Result<[u8; 32], ApiError> {
    if value.len() != MAX_DIGEST_HEX_LEN
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(ApiError::invalid());
    }
    let mut digest = [0_u8; 32];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        let high = hex_nibble(pair[0]).ok_or_else(ApiError::invalid)?;
        let low = hex_nibble(pair[1]).ok_or_else(ApiError::invalid)?;
        digest[index] = (high << 4) | low;
    }
    Ok(digest)
}

fn encode_digest(value: &[u8; 32]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(64);
    for byte in value {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: &'static str,
    retryable: bool,
}

impl ApiError {
    fn v2_disabled() -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            code: "MIGRATION_PACKAGE_V2_UNAVAILABLE",
            message: "Server-verified migration uploads are not configured.",
            retryable: false,
        }
    }
    fn invalid() -> Self {
        Self {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            code: "INVALID_MIGRATION_REQUEST",
            message: "The migration request is invalid.",
            retryable: false,
        }
    }

    fn too_large() -> Self {
        Self {
            status: StatusCode::PAYLOAD_TOO_LARGE,
            code: "MIGRATION_BATCH_TOO_LARGE",
            message: "The migration batch exceeds the API limit.",
            retryable: false,
        }
    }

    fn from_json(error: JsonRejection) -> Self {
        if error.status() == StatusCode::PAYLOAD_TOO_LARGE {
            Self::too_large()
        } else {
            Self {
                status: StatusCode::BAD_REQUEST,
                code: "INVALID_JSON",
                message: "The request body is not valid migration JSON.",
                retryable: false,
            }
        }
    }

    fn from_migration(error: MigrationError) -> Self {
        match error {
            MigrationError::AccessDenied(_) => Self {
                status: StatusCode::FORBIDDEN,
                code: "ACCESS_DENIED",
                message: "The requested migration operation is not permitted.",
                retryable: false,
            },
            MigrationError::InvalidBatch
            | MigrationError::RecordTooLarge
            | MigrationError::DuplicateRecord => Self::invalid(),
            MigrationError::InvalidDisposition => Self::invalid(),
            MigrationError::PackageV2Disabled => Self::v2_disabled(),
            MigrationError::PackageNotFound => Self {
                status: StatusCode::NOT_FOUND,
                code: "MIGRATION_PACKAGE_NOT_FOUND",
                message: "The migration package was not found.",
                retryable: false,
            },
            MigrationError::PackageConflict | MigrationError::PackageVerificationFailed => Self {
                status: StatusCode::CONFLICT,
                code: "MIGRATION_PACKAGE_CONFLICT",
                message: "The migration package state or commitment conflicts with this request.",
                retryable: false,
            },
            MigrationError::PackageQuotaExceeded => Self {
                status: StatusCode::INSUFFICIENT_STORAGE,
                code: "MIGRATION_STAGE_QUOTA_EXCEEDED",
                message: "The migration staging quota is exhausted.",
                retryable: false,
            },
            MigrationError::ChecksumMismatch => Self {
                status: StatusCode::UNPROCESSABLE_ENTITY,
                code: "MIGRATION_CHECKSUM_MISMATCH",
                message: "The migration digest does not match the supplied package.",
                retryable: false,
            },
            MigrationError::CheckpointConflict => Self {
                status: StatusCode::CONFLICT,
                code: "MIGRATION_CHECKPOINT_CONFLICT",
                message: "The migration checkpoint conflicts with this request.",
                retryable: false,
            },
            MigrationError::UnsupportedRecord => Self {
                status: StatusCode::UNPROCESSABLE_ENTITY,
                code: "MIGRATION_RECORD_UNSUPPORTED",
                message: "The batch contains a source row that cannot be represented safely.",
                retryable: false,
            },
            MigrationError::IdentityCollision => Self {
                status: StatusCode::CONFLICT,
                code: "MIGRATION_IDENTITY_CONFLICT",
                message: "The migration conflicts with an existing scoped identity.",
                retryable: false,
            },
            MigrationError::ReverseBlocked => Self {
                status: StatusCode::CONFLICT,
                code: "MIGRATION_REVERSE_BLOCKED",
                message: "The migration cannot be safely withdrawn in its current state.",
                retryable: false,
            },
            MigrationError::CommitFailed => Self {
                status: StatusCode::SERVICE_UNAVAILABLE,
                code: "MIGRATION_UNAVAILABLE",
                message: "The migration operation could not be completed.",
                retryable: true,
            },
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        (
            self.status,
            no_store(),
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
    use axum::{body::Body, http::Request};
    use litecr_application::{
        access::{
            AccessAuthenticator, AccessDirectory, AccessDirectoryError, AccessGrant,
            AccessIdentity, AccessService, AccessServiceConfig, AccessSessionRepository, ActorKind,
            ActorRef, OidcAuthorization, OidcFlow, OidcFlowSecrets, OidcIdentityClaims,
            OidcProvider, OidcProviderError, Permission, Role, SessionCredentials, SessionError,
        },
        migration::{
            BatchCommit, MigrationCheckpoint, MigrationError, MigrationExecutor, MigrationRun,
            QuarantineDisposition, QuarantineDispositionReceipt, ReverseManifest, ReversePreflight,
            ReversePreflightRequest, ReverseReceipt,
        },
    };
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use tower::ServiceExt;

    #[derive(Default)]
    struct FakeExecutor {
        commits: AtomicUsize,
        validations: AtomicUsize,
        reverse_preflights: AtomicUsize,
        dispositions: AtomicUsize,
    }

    #[tokio::test]
    async fn v2_routes_are_absent_from_the_frozen_v1_router_by_default() {
        let api = router(Arc::new(MigrationService::new(Box::new(
            FakeExecutor::default(),
        ))));
        let response = api
            .oneshot(
                Request::builder()
                    .uri("/api/v2/migration-packages")
                    .method("POST")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[async_trait]
    impl MigrationExecutor for FakeExecutor {
        async fn record_quarantine_disposition(
            &self,
            _context: &AccessContext,
            request: &QuarantineDisposition,
            _idempotency_hash: [u8; 32],
            _request_fingerprint: [u8; 32],
        ) -> Result<QuarantineDispositionReceipt, MigrationError> {
            self.dispositions.fetch_add(1, Ordering::SeqCst);
            Ok(QuarantineDispositionReceipt {
                disposition_id: "synthetic-disposition-id".into(),
                action: request.action,
                replacement_manifest_sha256: request.replacement_manifest_sha256,
                correction_count: request.corrections.len() as u64,
                replayed: false,
            })
        }

        async fn validate_batch(
            &self,
            _context: &AccessContext,
            batch: &MigrationBatch,
        ) -> Result<(), MigrationError> {
            self.validations.fetch_add(1, Ordering::SeqCst);
            litecr_application::migration::validate_batch(batch)
        }

        async fn commit_batch_atomically(
            &self,
            _context: &AccessContext,
            batch: &MigrationBatch,
        ) -> Result<BatchCommit, MigrationError> {
            self.commits.fetch_add(1, Ordering::SeqCst);
            Ok(BatchCommit {
                checkpoint: MigrationCheckpoint {
                    manifest_sha256: batch.run.manifest_sha256,
                    committed_batch_sequence: batch.sequence,
                    committed_record_count: batch.records.len() as u64,
                    state: MigrationRunState::Completed,
                },
                applied: Vec::new(),
            })
        }

        async fn checkpoint(
            &self,
            _context: &AccessContext,
            run: &MigrationRun,
        ) -> Result<Option<MigrationCheckpoint>, MigrationError> {
            Ok(Some(MigrationCheckpoint {
                manifest_sha256: run.manifest_sha256,
                committed_batch_sequence: 1,
                committed_record_count: 1,
                state: MigrationRunState::Completed,
            }))
        }

        async fn reverse_manifest_atomically(
            &self,
            _context: &AccessContext,
            request: &ReverseManifest,
        ) -> Result<ReverseReceipt, MigrationError> {
            Ok(ReverseReceipt {
                manifest_sha256: request.manifest_sha256,
                withdrawn_record_count: 1,
                detached_identifier_count: 0,
                retained_alias_count: 1,
                state: MigrationRunState::Reversed,
            })
        }

        async fn reverse_preflight(
            &self,
            _context: &AccessContext,
            _request: &ReversePreflightRequest,
        ) -> Result<ReversePreflight, MigrationError> {
            self.reverse_preflights.fetch_add(1, Ordering::SeqCst);
            Ok(ReversePreflight {
                eligible: false,
                blockers: vec![ReverseBlockerCount {
                    code: litecr_application::migration::ReverseBlockerCode::IdentityDependency,
                    count: 2,
                }],
            })
        }
    }

    struct TestAuthenticator {
        migration_write: bool,
        registry: &'static str,
        organization: &'static str,
    }

    #[async_trait]
    impl AccessAuthenticator for TestAuthenticator {
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
            if !matches!(
                opaque_credential,
                "valid-test-credential" | "read-test-credential"
            ) {
                return Err(SessionError::Invalid);
            }
            Ok(AccessIdentity {
                actor: ActorRef {
                    kind: ActorKind::IntegrationClient,
                    id: "test-client".into(),
                    issuer_url: None,
                },
                active: true,
                grants: vec![AccessGrant {
                    registry_id: self.registry.into(),
                    organization_id: self.organization.into(),
                    record_ids: None,
                    roles: vec![Role::IntegrationClient],
                    permissions: match opaque_credential {
                        "read-test-credential" => vec![Permission::MigrationRead],
                        "valid-test-credential" if self.migration_write => {
                            vec![Permission::MigrationWrite]
                        }
                        _ => vec![],
                    },
                }],
            })
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
    impl litecr_application::access::OidcFlowStore for UnusedFlows {
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
            Err(SessionError::Unavailable)
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
        ) -> Result<Option<litecr_application::access::AccessSession>, SessionError> {
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

    struct UnusedDirectory;

    #[async_trait]
    impl AccessDirectory for UnusedDirectory {
        async fn load_identity(
            &self,
            _kind: ActorKind,
            _issuer_url: Option<&str>,
            _external_subject: &str,
        ) -> Result<Option<AccessIdentity>, AccessDirectoryError> {
            Err(AccessDirectoryError::Unavailable)
        }
    }

    fn protected_router(executor: Arc<FakeExecutor>, migration_write: bool) -> Router {
        let access = Arc::new(
            AccessService::new(
                Arc::new(UnusedProvider),
                Arc::new(UnusedFlows),
                Arc::new(UnusedSessions),
                Arc::new(UnusedDirectory),
                Arc::new(TestAuthenticator {
                    migration_write,
                    registry: "registry-a",
                    organization: "org-a",
                }),
                Arc::new(litecr_application::access::NoopAccessSecurityAudit),
                AccessServiceConfig {
                    login_flow_lifetime_seconds: 300,
                    session_lifetime_seconds: 3600,
                },
            )
            .expect("valid access test configuration"),
        );
        let api = router(Arc::new(MigrationService::new(Box::new(
            FakeExecutorWrapper(executor),
        ))));
        crate::access::protected_routes(
            api,
            access,
            crate::access::AccessApiConfig {
                expected_origin: "http://localhost".into(),
                secure_cookies: false,
                environment: crate::access::AccessEnvironment::Development,
                session_lifetime_seconds: 3600,
                flow_lifetime_seconds: 300,
            },
        )
    }

    // Keep state sharing available to assertions while satisfying the service
    // trait's owned boxed executor interface.
    struct FakeExecutorWrapper(Arc<FakeExecutor>);

    #[async_trait]
    impl MigrationExecutor for FakeExecutorWrapper {
        async fn record_quarantine_disposition(
            &self,
            context: &AccessContext,
            request: &QuarantineDisposition,
            idempotency_hash: [u8; 32],
            request_fingerprint: [u8; 32],
        ) -> Result<QuarantineDispositionReceipt, MigrationError> {
            self.0
                .record_quarantine_disposition(
                    context,
                    request,
                    idempotency_hash,
                    request_fingerprint,
                )
                .await
        }

        async fn validate_batch(
            &self,
            context: &AccessContext,
            batch: &MigrationBatch,
        ) -> Result<(), MigrationError> {
            self.0.validate_batch(context, batch).await
        }

        async fn commit_batch_atomically(
            &self,
            context: &AccessContext,
            batch: &MigrationBatch,
        ) -> Result<BatchCommit, MigrationError> {
            self.0.commit_batch_atomically(context, batch).await
        }

        async fn checkpoint(
            &self,
            context: &AccessContext,
            run: &MigrationRun,
        ) -> Result<Option<MigrationCheckpoint>, MigrationError> {
            self.0.checkpoint(context, run).await
        }

        async fn reverse_manifest_atomically(
            &self,
            context: &AccessContext,
            request: &ReverseManifest,
        ) -> Result<ReverseReceipt, MigrationError> {
            self.0.reverse_manifest_atomically(context, request).await
        }

        async fn reverse_preflight(
            &self,
            context: &AccessContext,
            request: &ReversePreflightRequest,
        ) -> Result<ReversePreflight, MigrationError> {
            self.0.reverse_preflight(context, request).await
        }
    }

    fn request(method: &str, uri: &str, body: String, credential: Option<&str>) -> Request<Body> {
        let mut builder = Request::builder().method(method).uri(uri);
        if let Some(credential) = credential {
            builder = builder.header(header::AUTHORIZATION, format!("Bearer {credential}"));
        }
        builder
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body))
            .unwrap()
    }

    fn sample_batch(registry_id: &str, organization_id: &str) -> String {
        let raw = "{\"assertions\":[],\"identifiers\":[],\"source_record_key\":\"record-a\",\"source_system_id\":\"source-a\",\"source_version\":\"v1\"}\n";
        serde_json::json!({
            "run": {
                "manifest_sha256": "11".repeat(32),
                "stream_sha256": "22".repeat(32),
                "snapshot_id":"snapshot-a",
                "source_system_id":"source-a",
                "registry_id":registry_id,
                "organization_id":organization_id,
                "expected_record_count":1
            },
            "sequence":1,
            "first_input_line":1,
            "last_input_line":1,
            "batch_sha256":"41a618311c2728b997b298b5417e1bf8979e943e6e5ca01c5e5ce22f56153f88",
            "records":[{
                "source_system_id":"source-a",
                "source_record_key":"record-a",
                "source_version":"v1",
                "payload_sha256":"2711d0913981c9bb37c90190c6e0296ac48c05fcde808083ee48726e251415c8",
                "raw_ndjson":raw
            }]
        })
        .to_string()
    }

    #[tokio::test]
    async fn protected_batch_rejects_missing_auth_and_missing_migration_write() {
        let executor = Arc::new(FakeExecutor::default());
        let app = protected_router(executor.clone(), false);
        let no_auth = app
            .clone()
            .oneshot(request(
                "POST",
                "/api/v1/migrations/batches",
                sample_batch("registry-a", "org-a"),
                None,
            ))
            .await
            .unwrap();
        assert_eq!(no_auth.status(), StatusCode::UNAUTHORIZED);

        let preview_no_auth = app
            .clone()
            .oneshot(request(
                "POST",
                "/api/v1/migrations/batches/validate",
                sample_batch("registry-a", "org-a"),
                None,
            ))
            .await
            .unwrap();
        assert_eq!(preview_no_auth.status(), StatusCode::UNAUTHORIZED);

        let denied = app
            .clone()
            .oneshot(request(
                "POST",
                "/api/v1/migrations/batches",
                sample_batch("registry-a", "org-a"),
                Some("valid-test-credential"),
            ))
            .await
            .unwrap();
        assert_eq!(denied.status(), StatusCode::FORBIDDEN);
        assert_eq!(executor.commits.load(Ordering::SeqCst), 0);

        let preview_denied = app
            .clone()
            .oneshot(request(
                "POST",
                "/api/v1/migrations/batches/validate",
                sample_batch("registry-a", "org-a"),
                Some("valid-test-credential"),
            ))
            .await
            .unwrap();
        assert_eq!(preview_denied.status(), StatusCode::FORBIDDEN);
        assert_eq!(executor.validations.load(Ordering::SeqCst), 0);
        let reverse_preview_denied = app
            .oneshot(request(
                "POST",
                "/api/v1/migrations/reverse/preflight",
                serde_json::json!({
                    "manifest_sha256": "11".repeat(32),
                    "registry_id": "registry-a",
                    "organization_id": "org-a"
                })
                .to_string(),
                Some("valid-test-credential"),
            ))
            .await
            .unwrap();
        assert_eq!(reverse_preview_denied.status(), StatusCode::FORBIDDEN);
        assert_eq!(executor.reverse_preflights.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn protected_batch_rejects_cross_scope_and_accepts_exact_retry_without_values() {
        let executor = Arc::new(FakeExecutor::default());
        let app = protected_router(executor.clone(), true);
        let cross_scope = app
            .clone()
            .oneshot(request(
                "POST",
                "/api/v1/migrations/batches",
                sample_batch("registry-b", "org-b"),
                Some("valid-test-credential"),
            ))
            .await
            .unwrap();
        assert_eq!(cross_scope.status(), StatusCode::FORBIDDEN);
        assert_eq!(executor.commits.load(Ordering::SeqCst), 0);

        let preview_cross_scope = app
            .clone()
            .oneshot(request(
                "POST",
                "/api/v1/migrations/batches/validate",
                sample_batch("registry-b", "org-b"),
                Some("valid-test-credential"),
            ))
            .await
            .unwrap();
        assert_eq!(preview_cross_scope.status(), StatusCode::FORBIDDEN);
        assert_eq!(executor.validations.load(Ordering::SeqCst), 0);

        let preview = app
            .clone()
            .oneshot(request(
                "POST",
                "/api/v1/migrations/batches/validate",
                sample_batch("registry-a", "org-a"),
                Some("valid-test-credential"),
            ))
            .await
            .unwrap();
        assert_eq!(preview.status(), StatusCode::NO_CONTENT);
        assert_eq!(preview.headers()[header::CACHE_CONTROL], "no-store");
        assert_eq!(executor.validations.load(Ordering::SeqCst), 1);
        assert_eq!(executor.commits.load(Ordering::SeqCst), 0);

        let mut first_receipt = None;
        for _ in 0..2 {
            let response = app
                .clone()
                .oneshot(request(
                    "POST",
                    "/api/v1/migrations/batches",
                    sample_batch("registry-a", "org-a"),
                    Some("valid-test-credential"),
                ))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::ACCEPTED);
            assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
            let body = axum::body::to_bytes(response.into_body(), MAX_JSON_BODY_BYTES)
                .await
                .unwrap();
            let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(json["data"]["committed_batch_sequence"], 1);
            assert!(String::from_utf8_lossy(&body).find("record-a").is_none());
            if let Some(prior) = &first_receipt {
                assert_eq!(&json["data"], prior);
            } else {
                first_receipt = Some(json["data"].clone());
            }
        }
        assert_eq!(executor.commits.load(Ordering::SeqCst), 2);

        let run = serde_json::from_str::<serde_json::Value>(&sample_batch("registry-a", "org-a"))
            .unwrap()["run"]
            .clone();
        let status = app
            .clone()
            .oneshot(request(
                "POST",
                "/api/v1/migrations/checkpoint",
                serde_json::json!({"run":run}).to_string(),
                Some("valid-test-credential"),
            ))
            .await
            .unwrap();
        assert_eq!(status.status(), StatusCode::OK);
        let status_body = axum::body::to_bytes(status.into_body(), 4096)
            .await
            .unwrap();
        let status_json: serde_json::Value = serde_json::from_slice(&status_body).unwrap();
        assert_eq!(status_json["data"]["state"], "completed");
        assert_eq!(status_json["data"]["committed_record_count"], 1);

        let reverse = serde_json::json!({
            "manifest_sha256":"11".repeat(32),
            "confirm_manifest_sha256":"11".repeat(32),
            "registry_id":"registry-a",
            "organization_id":"org-a",
            "reason":"approved synthetic withdrawal"
        });
        let response = app
            .oneshot(request(
                "POST",
                "/api/v1/migrations/reverse",
                reverse.to_string(),
                Some("valid-test-credential"),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let reverse_body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .unwrap();
        let reverse_json: serde_json::Value = serde_json::from_slice(&reverse_body).unwrap();
        assert_eq!(reverse_json["data"]["state"], "reversed");
        assert_eq!(reverse_json["data"]["withdrawn_record_count"], 1);
        assert!(!String::from_utf8_lossy(&reverse_body).contains("record-a"));
    }

    #[tokio::test]
    async fn reverse_preflight_is_scoped_read_only_and_redacted() {
        let executor = Arc::new(FakeExecutor::default());
        let app = protected_router(executor.clone(), true);
        let request_body = serde_json::json!({
            "manifest_sha256": "11".repeat(32),
            "registry_id": "registry-a",
            "organization_id": "org-a"
        })
        .to_string();

        let denied = app.clone().oneshot(request(
            "POST", "/api/v1/migrations/reverse/preflight",
            serde_json::json!({"manifest_sha256":"11".repeat(32),"registry_id":"registry-b","organization_id":"org-b"}).to_string(),
            Some("valid-test-credential"),
        )).await.unwrap();
        assert_eq!(denied.status(), StatusCode::FORBIDDEN);
        assert_eq!(executor.reverse_preflights.load(Ordering::SeqCst), 0);

        let response = app
            .oneshot(request(
                "POST",
                "/api/v1/migrations/reverse/preflight",
                request_body,
                Some("valid-test-credential"),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["data"]["eligible"], false);
        assert_eq!(json["data"]["blockers"][0]["code"], "identity_dependency");
        assert_eq!(json["data"]["blockers"][0]["count"], 2);
        assert!(!String::from_utf8_lossy(&body).contains("source_record"));
        assert!(!String::from_utf8_lossy(&body).contains("patient-value"));
        assert_eq!(executor.reverse_preflights.load(Ordering::SeqCst), 1);
        assert_eq!(executor.commits.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn migration_reader_can_validate_and_inspect_but_cannot_mutate() {
        let executor = Arc::new(FakeExecutor::default());
        let app = protected_router(executor.clone(), true);
        let run = serde_json::from_str::<serde_json::Value>(&sample_batch("registry-a", "org-a"))
            .unwrap()["run"]
            .clone();
        let checkpoint = app
            .clone()
            .oneshot(request(
                "POST",
                "/api/v1/migrations/checkpoint",
                serde_json::json!({"run":run}).to_string(),
                Some("read-test-credential"),
            ))
            .await
            .unwrap();
        assert_eq!(checkpoint.status(), StatusCode::OK);

        let preflight = app
            .clone()
            .oneshot(request(
                "POST",
                "/api/v1/migrations/reverse/preflight",
                serde_json::json!({
                    "manifest_sha256":"11".repeat(32),
                    "registry_id":"registry-a",
                    "organization_id":"org-a"
                })
                .to_string(),
                Some("read-test-credential"),
            ))
            .await
            .unwrap();
        assert_eq!(preflight.status(), StatusCode::OK);

        let validate = app
            .clone()
            .oneshot(request(
                "POST",
                "/api/v1/migrations/batches/validate",
                sample_batch("registry-a", "org-a"),
                Some("read-test-credential"),
            ))
            .await
            .unwrap();
        assert_eq!(validate.status(), StatusCode::NO_CONTENT);
        assert_eq!(executor.validations.load(Ordering::SeqCst), 1);

        let apply = app
            .clone()
            .oneshot(request(
                "POST",
                "/api/v1/migrations/batches",
                sample_batch("registry-a", "org-a"),
                Some("read-test-credential"),
            ))
            .await
            .unwrap();
        assert_eq!(apply.status(), StatusCode::FORBIDDEN);
        assert_eq!(executor.commits.load(Ordering::SeqCst), 0);

        let disposition = app
            .clone()
            .oneshot(request(
                "POST",
                "/api/v1/migrations/quarantine-dispositions",
                serde_json::json!({
                    "action":"corrected_resubmission",
                    "parent_manifest_sha256":"11".repeat(32),
                    "replacement_manifest_sha256":"22".repeat(32),
                    "registry_id":"registry-a",
                    "organization_id":"org-a",
                    "reason":"read-only operator cannot disposition quarantine",
                    "idempotency_key":"synthetic-reader-disposition",
                    "corrections":[{
                        "input_line":4,
                        "quarantine_code":"ROW_INVALID_UTF8_OR_JSON",
                        "original_payload_sha256":"33".repeat(32),
                        "corrected_payload_sha256":"44".repeat(32)
                    }]
                })
                .to_string(),
                Some("read-test-credential"),
            ))
            .await
            .unwrap();
        assert_eq!(disposition.status(), StatusCode::FORBIDDEN);
        assert_eq!(executor.dispositions.load(Ordering::SeqCst), 0);

        let reverse = app
            .oneshot(request(
                "POST",
                "/api/v1/migrations/reverse",
                serde_json::json!({
                    "manifest_sha256":"11".repeat(32),
                    "confirm_manifest_sha256":"11".repeat(32),
                    "registry_id":"registry-a",
                    "organization_id":"org-a",
                    "reason":"reader is not approved to change import state"
                })
                .to_string(),
                Some("read-test-credential"),
            ))
            .await
            .unwrap();
        assert_eq!(reverse.status(), StatusCode::FORBIDDEN);
        assert_eq!(executor.reverse_preflights.load(Ordering::SeqCst), 1);
        assert_eq!(executor.commits.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn strict_input_and_reverse_confirmation_are_rejected_without_executor_calls() {
        let executor = Arc::new(FakeExecutor::default());
        let app = protected_router(executor.clone(), true);
        let mut invalid: serde_json::Value =
            serde_json::from_str(&sample_batch("registry-a", "org-a")).unwrap();
        invalid["unexpected"] = serde_json::json!("patient-value-must-not-echo");
        let response = app
            .clone()
            .oneshot(request(
                "POST",
                "/api/v1/migrations/batches",
                invalid.to_string(),
                Some("valid-test-credential"),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .unwrap();
        assert!(!String::from_utf8_lossy(&body).contains("patient-value"));

        let reverse = serde_json::json!({
            "manifest_sha256":"11".repeat(32),
            "confirm_manifest_sha256":"22".repeat(32),
            "registry_id":"registry-a",
            "organization_id":"org-a",
            "reason":"operator-approved withdrawal"
        });
        let response = app
            .oneshot(request(
                "POST",
                "/api/v1/migrations/reverse",
                reverse.to_string(),
                Some("valid-test-credential"),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(executor.commits.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn quarantine_disposition_requires_scope_and_rejects_relationship_resolution() {
        let executor = Arc::new(FakeExecutor::default());
        let app = protected_router(executor.clone(), true);
        let body = serde_json::json!({
            "action": "corrected_resubmission",
            "parent_manifest_sha256": "11".repeat(32),
            "replacement_manifest_sha256": "22".repeat(32),
            "registry_id": "registry-a",
            "organization_id": "org-a",
            "reason": "reviewed mechanical encoding correction",
            "idempotency_key": "synthetic-disposition-1",
            "corrections": [{
                "input_line": 4,
                "quarantine_code": "ROW_INVALID_UTF8_OR_JSON",
                "original_payload_sha256": "33".repeat(32),
                "corrected_payload_sha256": "44".repeat(32)
            }]
        })
        .to_string();

        let unauthenticated = app
            .clone()
            .oneshot(request(
                "POST",
                "/api/v1/migrations/quarantine-dispositions",
                body.clone(),
                None,
            ))
            .await
            .unwrap();
        assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED);

        let wrong_scope_body = body
            .replace("registry-a", "registry-b")
            .replace("org-a", "org-b");
        let wrong_scope = app
            .clone()
            .oneshot(request(
                "POST",
                "/api/v1/migrations/quarantine-dispositions",
                wrong_scope_body,
                Some("valid-test-credential"),
            ))
            .await
            .unwrap();
        assert_eq!(wrong_scope.status(), StatusCode::FORBIDDEN);
        assert_eq!(executor.dispositions.load(Ordering::SeqCst), 0);

        let accepted = app
            .clone()
            .oneshot(request(
                "POST",
                "/api/v1/migrations/quarantine-dispositions",
                body,
                Some("valid-test-credential"),
            ))
            .await
            .unwrap();
        assert_eq!(accepted.status(), StatusCode::OK);
        assert_eq!(accepted.headers()[header::CACHE_CONTROL], "no-store");
        let accepted_body = axum::body::to_bytes(accepted.into_body(), 4096)
            .await
            .unwrap();
        let accepted_json: serde_json::Value = serde_json::from_slice(&accepted_body).unwrap();
        assert_eq!(accepted_json["data"]["action"], "corrected_resubmission");
        assert_eq!(accepted_json["data"]["correction_count"], 1);
        assert_eq!(executor.dispositions.load(Ordering::SeqCst), 1);

        let forbidden = serde_json::json!({
            "action": "corrected_resubmission",
            "parent_manifest_sha256": "11".repeat(32),
            "replacement_manifest_sha256": "22".repeat(32),
            "registry_id": "registry-a",
            "organization_id": "org-a",
            "reason": "attempt to drop unsupported relationship",
            "idempotency_key": "synthetic-disposition-2",
            "corrections": [{
                "input_line": 4,
                "quarantine_code": "HISTORICAL_RELATIONSHIPS_UNSUPPORTED",
                "original_payload_sha256": "33".repeat(32),
                "corrected_payload_sha256": "44".repeat(32)
            }]
        })
        .to_string();
        let denied = app
            .oneshot(request(
                "POST",
                "/api/v1/migrations/quarantine-dispositions",
                forbidden,
                Some("valid-test-credential"),
            ))
            .await
            .unwrap();
        assert_eq!(denied.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(executor.dispositions.load(Ordering::SeqCst), 1);
    }
}
