use axum::{
    Extension, Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post, put},
};
use litecr_application::{
    access::{AccessContext, ResourceScope},
    localization::{
        CatalogError, CatalogService, DraftTranslation, Locale, MessageSource, ReviewDecision,
        TranslationState,
    },
};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, sync::Arc};

#[derive(Clone)]
struct StateData {
    service: Arc<CatalogService>,
}
#[derive(Deserialize)]
struct LocaleInput {
    tag: String,
    direction: String,
}
#[derive(Deserialize)]
struct SourceInput {
    text: String,
    revision: i64,
    placeholders: BTreeSet<String>,
    plural: bool,
}
#[derive(Deserialize)]
struct DraftInput {
    value: Option<String>,
    source_revision: i64,
    plural_forms: Option<std::collections::BTreeMap<String, String>>,
}
#[derive(Deserialize)]
struct PublishInput {
    expected_revision: Option<i64>,
}
#[derive(Deserialize)]
struct RollbackInput {
    target_revision: i64,
    expected_revision: Option<i64>,
}
#[derive(Serialize)]
struct ApiError {
    code: &'static str,
    message: &'static str,
}
#[derive(Serialize)]
struct LocaleBody {
    tag: String,
    direction: String,
    enabled: bool,
}
impl From<Locale> for LocaleBody {
    fn from(v: Locale) -> Self {
        Self {
            tag: v.tag,
            direction: v.direction,
            enabled: v.enabled,
        }
    }
}
#[derive(Serialize)]
struct DraftBody {
    locale: String,
    namespace: String,
    key: String,
    value: String,
    plural_forms: Option<std::collections::BTreeMap<String, String>>,
    source_revision: i64,
    state: &'static str,
    stale: bool,
}
#[derive(Serialize)]
struct CatalogEntryBody {
    key: String,
    source_text: String,
    source_revision: i64,
    placeholders: BTreeSet<String>,
    plural: bool,
    draft: Option<DraftBody>,
}
impl From<DraftTranslation> for DraftBody {
    fn from(v: DraftTranslation) -> Self {
        Self {
            locale: v.locale,
            namespace: v.namespace,
            key: v.key,
            value: v.value,
            plural_forms: v.plural_forms,
            source_revision: v.source_revision,
            state: match v.state {
                TranslationState::Draft => "draft",
                TranslationState::InReview => "in_review",
                TranslationState::Approved => "approved",
            },
            stale: v.stale,
        }
    }
}
#[derive(Serialize)]
struct BundleBody {
    locale: String,
    namespace: String,
    revision: i64,
    entries: std::collections::BTreeMap<String, String>,
    fallback_locale: String,
}
impl From<litecr_application::localization::PublishedBundle> for BundleBody {
    fn from(v: litecr_application::localization::PublishedBundle) -> Self {
        Self {
            locale: v.locale,
            namespace: v.namespace,
            revision: v.revision,
            entries: v.entries,
            fallback_locale: v.fallback_locale,
        }
    }
}
fn scope(registry: String, organization: String) -> ResourceScope {
    ResourceScope {
        registry_id: registry,
        organization_id: organization,
        record_id: None,
    }
}
fn error(e: CatalogError) -> (StatusCode, Json<ApiError>) {
    let (status, code) = match e {
        CatalogError::Denied(_) => (StatusCode::FORBIDDEN, "ACCESS_DENIED"),
        CatalogError::Invalid => (StatusCode::BAD_REQUEST, "INVALID_CONFIG"),
        CatalogError::Conflict => (StatusCode::CONFLICT, "VERSION_CONFLICT"),
        CatalogError::NotFound => (StatusCode::NOT_FOUND, "NOT_FOUND"),
        CatalogError::Unavailable => (StatusCode::SERVICE_UNAVAILABLE, "CONFIG_UNAVAILABLE"),
    };
    (
        status,
        Json(ApiError {
            code,
            message: "Catalog request could not be completed.",
        }),
    )
}
pub fn protected_router(service: Arc<CatalogService>) -> Router {
    Router::new()
        .route(
            "/api/v1/config/{registry}/{organization}/locales",
            get(locales).post(add_locale),
        )
        .route(
            "/api/v1/config/{registry}/{organization}/locales/{locale}/{namespace}/entries",
            get(entries),
        )
        .route(
            "/api/v1/config/{registry}/{organization}/sources/{namespace}/{key}",
            put(source),
        )
        .route(
            "/api/v1/config/{registry}/{organization}/locales/{locale}/{namespace}/{key}",
            put(draft).get(get_draft),
        )
        .route(
            "/api/v1/config/{registry}/{organization}/locales/{locale}/{namespace}/{key}/review",
            post(review),
        )
        .route(
            "/api/v1/config/{registry}/{organization}/locales/{locale}/{namespace}/publish",
            post(publish),
        )
        .route(
            "/api/v1/config/{registry}/{organization}/locales/{locale}/{namespace}/rollback",
            post(rollback),
        )
        .with_state(StateData { service })
}

pub fn public_router(service: Arc<CatalogService>) -> Router {
    Router::new()
        .route(
            "/api/v1/bundles/{registry}/{organization}/locales",
            get(published_locales),
        )
        .route(
            "/api/v1/bundles/{registry}/{organization}/{locale}/{namespace}",
            get(bundle),
        )
        .with_state(StateData { service })
}
async fn published_locales(
    State(s): State<StateData>,
    Path((r, o)): Path<(String, String)>,
) -> Result<Json<Vec<LocaleBody>>, (StatusCode, Json<ApiError>)> {
    s.service
        .published_ui_locales(&scope(r, o))
        .await
        .map(|locales| Json(locales.into_iter().map(Into::into).collect()))
        .map_err(error)
}
async fn locales(
    State(s): State<StateData>,
    Path((r, o)): Path<(String, String)>,
    ctx: Option<Extension<AccessContext>>,
) -> Result<Json<Vec<LocaleBody>>, (StatusCode, Json<ApiError>)> {
    s.service
        .locales(ctx.as_ref().map(|Extension(c)| c), &scope(r, o))
        .await
        .map(|v| Json(v.into_iter().map(Into::into).collect()))
        .map_err(error)
}
async fn add_locale(
    State(s): State<StateData>,
    Path((r, o)): Path<(String, String)>,
    ctx: Option<Extension<AccessContext>>,
    Json(body): Json<LocaleInput>,
) -> Result<StatusCode, (StatusCode, Json<ApiError>)> {
    let locale = Locale {
        tag: body.tag,
        direction: body.direction,
        enabled: true,
    };
    s.service
        .add_locale(ctx.as_ref().map(|Extension(c)| c), &scope(r, o), locale)
        .await
        .map_err(error)?;
    Ok(StatusCode::CREATED)
}
async fn source(
    State(s): State<StateData>,
    Path((r, o, n, k)): Path<(String, String, String, String)>,
    ctx: Option<Extension<AccessContext>>,
    Json(b): Json<SourceInput>,
) -> Result<StatusCode, (StatusCode, Json<ApiError>)> {
    s.service
        .set_source(
            ctx.as_ref().map(|Extension(c)| c),
            &scope(r, o),
            MessageSource {
                namespace: n,
                key: k,
                source_text: b.text,
                source_revision: b.revision,
                placeholders: b.placeholders,
                plural: b.plural,
            },
        )
        .await
        .map_err(error)?;
    Ok(StatusCode::NO_CONTENT)
}
async fn draft(
    State(s): State<StateData>,
    Path((r, o, l, n, k)): Path<(String, String, String, String, String)>,
    ctx: Option<Extension<AccessContext>>,
    Json(b): Json<DraftInput>,
) -> Result<StatusCode, (StatusCode, Json<ApiError>)> {
    s.service
        .save_draft(
            ctx.as_ref().map(|Extension(c)| c),
            &scope(r, o),
            DraftTranslation {
                locale: l,
                namespace: n,
                key: k,
                value: match &b.plural_forms {
                    Some(forms) => forms
                        .get("other")
                        .cloned()
                        .ok_or_else(|| error(CatalogError::Invalid))?,
                    None => b.value.ok_or_else(|| error(CatalogError::Invalid))?,
                },
                plural_forms: b.plural_forms,
                source_revision: b.source_revision,
                state: TranslationState::Draft,
                stale: false,
            },
        )
        .await
        .map_err(error)?;
    Ok(StatusCode::NO_CONTENT)
}
async fn get_draft(
    State(s): State<StateData>,
    Path((r, o, l, n, k)): Path<(String, String, String, String, String)>,
    ctx: Option<Extension<AccessContext>>,
) -> Result<Json<Option<DraftBody>>, (StatusCode, Json<ApiError>)> {
    s.service
        .draft(ctx.as_ref().map(|Extension(c)| c), &scope(r, o), &l, &n, &k)
        .await
        .map(|v| Json(v.map(Into::into)))
        .map_err(error)
}
async fn entries(
    State(s): State<StateData>,
    Path((r, o, l, n)): Path<(String, String, String, String)>,
    ctx: Option<Extension<AccessContext>>,
) -> Result<Json<Vec<CatalogEntryBody>>, (StatusCode, Json<ApiError>)> {
    s.service
        .entries(ctx.as_ref().map(|Extension(c)| c), &scope(r, o), &l, &n)
        .await
        .map(|rows| {
            Json(
                rows.into_iter()
                    .map(|entry| CatalogEntryBody {
                        key: entry.source.key,
                        source_text: entry.source.source_text,
                        source_revision: entry.source.source_revision,
                        placeholders: entry.source.placeholders,
                        plural: entry.source.plural,
                        draft: entry.draft.map(Into::into),
                    })
                    .collect(),
            )
        })
        .map_err(error)
}
#[derive(Serialize)]
struct ReviewBody {
    decision: &'static str,
}
async fn review(
    State(s): State<StateData>,
    Path((r, o, l, n, k)): Path<(String, String, String, String, String)>,
    ctx: Option<Extension<AccessContext>>,
    Json(b): Json<ReviewInput>,
) -> Result<Json<ReviewBody>, (StatusCode, Json<ApiError>)> {
    let decision = match b.decision.as_str() {
        "approve" => ReviewDecision::Approve,
        "request_changes" => ReviewDecision::RequestChanges,
        _ => return Err(error(CatalogError::Invalid)),
    };
    s.service
        .review(
            ctx.as_ref().map(|Extension(c)| c),
            &scope(r, o),
            &l,
            &n,
            &k,
            decision,
        )
        .await
        .map_err(error)?;
    Ok(Json(ReviewBody {
        decision: "recorded",
    }))
}
#[derive(Deserialize)]
struct ReviewInput {
    decision: String,
}
async fn publish(
    State(s): State<StateData>,
    Path((r, o, l, n)): Path<(String, String, String, String)>,
    ctx: Option<Extension<AccessContext>>,
    Json(b): Json<PublishInput>,
) -> Result<Json<BundleBody>, (StatusCode, Json<ApiError>)> {
    s.service
        .publish(
            ctx.as_ref().map(|Extension(c)| c),
            &scope(r, o),
            &l,
            &n,
            b.expected_revision,
        )
        .await
        .map(|v| Json(v.into()))
        .map_err(error)
}
async fn rollback(
    State(s): State<StateData>,
    Path((r, o, l, n)): Path<(String, String, String, String)>,
    ctx: Option<Extension<AccessContext>>,
    Json(b): Json<RollbackInput>,
) -> Result<Json<BundleBody>, (StatusCode, Json<ApiError>)> {
    s.service
        .rollback(
            ctx.as_ref().map(|Extension(c)| c),
            &scope(r, o),
            &l,
            &n,
            b.target_revision,
            b.expected_revision,
        )
        .await
        .map(|v| Json(v.into()))
        .map_err(error)
}
async fn bundle(
    State(s): State<StateData>,
    Path((r, o, l, n)): Path<(String, String, String, String)>,
) -> Result<Json<Option<BundleBody>>, (StatusCode, Json<ApiError>)> {
    s.service
        .public_bundle(&scope(r, o), &l, &n)
        .await
        .map(|v| Json(v.map(Into::into)))
        .map_err(error)
}
