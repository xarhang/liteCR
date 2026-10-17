use axum::{
    Extension, Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post, put},
};
use litecr_application::{
    access::{AccessContext, ResourceScope},
    country_profiles::{
        CountryProfile, CountryProfileCoverage, CountryProfileService, ProfileError,
    },
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Clone)]
struct StateData {
    service: Arc<CountryProfileService>,
}
#[derive(Deserialize)]
struct ProfileInput {
    default_locale: String,
    calendar: String,
    timezone: String,
    address_fields: Vec<String>,
    identifier_types: Vec<String>,
    phone_region: String,
    expected_version: Option<i64>,
}
#[derive(Deserialize)]
struct PublishInput {
    expected_version: i64,
}
#[derive(Deserialize)]
struct RollbackInput {
    target_version: i64,
    expected_version: i64,
}
#[derive(Serialize)]
struct ProfileBody {
    country_code: String,
    default_locale: String,
    calendar: String,
    timezone: String,
    address_fields: Vec<String>,
    identifier_types: Vec<String>,
    phone_region: String,
    version: i64,
}
#[derive(Serialize)]
struct CoverageBody {
    country_code: String,
    latest_version: i64,
    active_version: Option<i64>,
}
impl From<CountryProfileCoverage> for CoverageBody {
    fn from(value: CountryProfileCoverage) -> Self {
        Self {
            country_code: value.country_code,
            latest_version: value.latest_version,
            active_version: value.active_version,
        }
    }
}
impl From<CountryProfile> for ProfileBody {
    fn from(p: CountryProfile) -> Self {
        Self {
            country_code: p.country_code,
            default_locale: p.default_locale,
            calendar: p.calendar,
            timezone: p.timezone,
            address_fields: p.address_fields,
            identifier_types: p.identifier_types,
            phone_region: p.phone_region,
            version: p.version,
        }
    }
}
#[derive(Serialize)]
struct ApiError {
    code: &'static str,
    message: &'static str,
}
fn scope(r: String, o: String) -> ResourceScope {
    ResourceScope {
        registry_id: r,
        organization_id: o,
        record_id: None,
    }
}
fn err(e: ProfileError) -> (StatusCode, Json<ApiError>) {
    let (s, c) = match e {
        ProfileError::Denied(_) => (StatusCode::FORBIDDEN, "ACCESS_DENIED"),
        ProfileError::Invalid => (StatusCode::BAD_REQUEST, "INVALID_CONFIG"),
        ProfileError::Conflict => (StatusCode::CONFLICT, "VERSION_CONFLICT"),
        ProfileError::NotFound => (StatusCode::NOT_FOUND, "NOT_FOUND"),
        ProfileError::Unavailable => (StatusCode::SERVICE_UNAVAILABLE, "CONFIG_UNAVAILABLE"),
    };
    (
        s,
        Json(ApiError {
            code: c,
            message: "Country profile request could not be completed.",
        }),
    )
}
pub fn protected_router(service: Arc<CountryProfileService>) -> Router {
    Router::new()
        .route(
            "/api/v1/config/{registry}/{organization}/country-profiles/{country}",
            put(save),
        )
        .route(
            "/api/v1/config/{registry}/{organization}/country-profiles/{country}/publish",
            post(publish),
        )
        .route(
            "/api/v1/config/{registry}/{organization}/country-profiles/{country}/rollback",
            post(rollback),
        )
        .with_state(StateData { service })
}
pub fn public_router(service: Arc<CountryProfileService>) -> Router {
    Router::new()
        .route(
            "/api/v1/config/{registry}/{organization}/country-profiles",
            get(coverage),
        )
        .route(
            "/api/v1/config/{registry}/{organization}/country-profiles/{country}",
            get(current),
        )
        .with_state(StateData { service })
}
async fn coverage(
    State(s): State<StateData>,
    Path((r, o)): Path<(String, String)>,
) -> Result<Json<Vec<CoverageBody>>, (StatusCode, Json<ApiError>)> {
    s.service
        .coverage(&scope(r, o))
        .await
        .map(|items| Json(items.into_iter().map(Into::into).collect()))
        .map_err(err)
}
async fn current(
    State(s): State<StateData>,
    Path((r, o, c)): Path<(String, String, String)>,
) -> Result<Json<Option<ProfileBody>>, (StatusCode, Json<ApiError>)> {
    s.service
        .current(&scope(r, o), &c)
        .await
        .map(|p| Json(p.map(Into::into)))
        .map_err(err)
}
async fn save(
    State(s): State<StateData>,
    Path((r, o, c)): Path<(String, String, String)>,
    ctx: Option<Extension<AccessContext>>,
    Json(b): Json<ProfileInput>,
) -> Result<StatusCode, (StatusCode, Json<ApiError>)> {
    let version = next_version(b.expected_version).map_err(err)?;
    let p = CountryProfile {
        country_code: c,
        default_locale: b.default_locale,
        calendar: b.calendar,
        timezone: b.timezone,
        address_fields: b.address_fields,
        identifier_types: b.identifier_types,
        phone_region: b.phone_region,
        matching_policy_ref: None,
        version,
    };
    s.service
        .save_draft(
            ctx.as_ref().map(|Extension(c)| c),
            &scope(r, o),
            p,
            b.expected_version,
        )
        .await
        .map_err(err)?;
    Ok(StatusCode::CREATED)
}

fn next_version(expected: Option<i64>) -> Result<i64, ProfileError> {
    expected
        .unwrap_or(0)
        .checked_add(1)
        .filter(|version| *version > 0)
        .ok_or(ProfileError::Invalid)
}

async fn publish(
    State(s): State<StateData>,
    Path((r, o, c)): Path<(String, String, String)>,
    ctx: Option<Extension<AccessContext>>,
    Json(b): Json<PublishInput>,
) -> Result<Json<ProfileBody>, (StatusCode, Json<ApiError>)> {
    s.service
        .publish(
            ctx.as_ref().map(|Extension(c)| c),
            &scope(r, o),
            &c,
            b.expected_version,
        )
        .await
        .map(|p| Json(p.into()))
        .map_err(err)
}
async fn rollback(
    State(s): State<StateData>,
    Path((r, o, c)): Path<(String, String, String)>,
    ctx: Option<Extension<AccessContext>>,
    Json(b): Json<RollbackInput>,
) -> Result<Json<ProfileBody>, (StatusCode, Json<ApiError>)> {
    s.service
        .rollback(
            ctx.as_ref().map(|Extension(c)| c),
            &scope(r, o),
            &c,
            b.target_version,
            b.expected_version,
        )
        .await
        .map(|p| Json(p.into()))
        .map_err(err)
}

#[cfg(test)]
mod version_tests {
    use super::*;
    #[test]
    fn invalid_versions_cannot_panic_or_wrap() {
        for version in [i64::MIN, -1, i64::MAX] {
            assert!(matches!(
                next_version(Some(version)),
                Err(ProfileError::Invalid)
            ));
        }
        assert_eq!(next_version(None).unwrap(), 1);
        assert_eq!(next_version(Some(1)).unwrap(), 2);
    }
}
