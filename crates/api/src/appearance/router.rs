use axum::{
    Extension, Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post, put},
};
use litecr_application::{
    access::{AccessContext, ResourceScope},
    appearance::{
        AppearanceError, AppearancePreference, AppearanceProfile, AppearanceService, ColorMode,
        Density, SemanticTokens,
    },
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Clone)]
struct StateData {
    service: Arc<AppearanceService>,
}
#[derive(Deserialize)]
struct ProfileInput {
    name: String,
    accent: String,
    on_accent: String,
    logo_asset_ref: Option<String>,
    color_mode: String,
    density: String,
    high_contrast: bool,
    tokens: TokenInput,
    expected_version: Option<i64>,
}
#[derive(Deserialize)]
struct TokenInput {
    canvas: String,
    surface: String,
    text: String,
    text_muted: String,
    action: String,
    on_action: String,
    control_border: String,
    focus: String,
}
#[derive(Serialize)]
struct TokenBody {
    canvas: String,
    surface: String,
    text: String,
    text_muted: String,
    action: String,
    on_action: String,
    control_border: String,
    focus: String,
}
impl From<SemanticTokens> for TokenBody {
    fn from(t: SemanticTokens) -> Self {
        Self {
            canvas: t.canvas,
            surface: t.surface,
            text: t.text,
            text_muted: t.text_muted,
            action: t.action,
            on_action: t.on_action,
            control_border: t.control_border,
            focus: t.focus,
        }
    }
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
#[derive(Deserialize)]
struct PreferenceInput {
    color_mode: Option<String>,
    density: Option<String>,
    high_contrast: Option<bool>,
    locale_tag: Option<String>,
}
#[derive(Serialize)]
struct PreferenceBody {
    color_mode: Option<&'static str>,
    density: Option<&'static str>,
    high_contrast: Option<bool>,
    locale_tag: Option<String>,
}
impl From<AppearancePreference> for PreferenceBody {
    fn from(p: AppearancePreference) -> Self {
        Self {
            color_mode: p.color_mode.map(|v| match v {
                ColorMode::Light => "light",
                ColorMode::Dark => "dark",
                ColorMode::System => "system",
            }),
            density: p.density.map(|v| match v {
                Density::Comfortable => "comfortable",
                Density::Compact => "compact",
            }),
            high_contrast: p.high_contrast,
            locale_tag: p.locale_tag,
        }
    }
}
#[derive(Serialize)]
struct ProfileBody {
    name: String,
    accent: String,
    on_accent: String,
    logo_asset_ref: Option<String>,
    color_mode: &'static str,
    density: &'static str,
    high_contrast: bool,
    tokens: TokenBody,
    version: i64,
}
impl From<AppearanceProfile> for ProfileBody {
    fn from(p: AppearanceProfile) -> Self {
        Self {
            name: p.name,
            accent: p.accent,
            on_accent: p.on_accent,
            logo_asset_ref: p.logo_asset_ref,
            color_mode: match p.color_mode {
                ColorMode::Light => "light",
                ColorMode::Dark => "dark",
                ColorMode::System => "system",
            },
            density: match p.density {
                Density::Comfortable => "comfortable",
                Density::Compact => "compact",
            },
            high_contrast: p.high_contrast,
            tokens: p.tokens.into(),
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
fn err(e: AppearanceError) -> (StatusCode, Json<ApiError>) {
    let (s, c) = match e {
        AppearanceError::Denied(_) => (StatusCode::FORBIDDEN, "ACCESS_DENIED"),
        AppearanceError::Invalid => (StatusCode::BAD_REQUEST, "INVALID_CONFIG"),
        AppearanceError::Conflict => (StatusCode::CONFLICT, "VERSION_CONFLICT"),
        AppearanceError::NotFound => (StatusCode::NOT_FOUND, "NOT_FOUND"),
        AppearanceError::Unavailable => (StatusCode::SERVICE_UNAVAILABLE, "CONFIG_UNAVAILABLE"),
    };
    (
        s,
        Json(ApiError {
            code: c,
            message: "Appearance request could not be completed.",
        }),
    )
}
pub fn protected_router(service: Arc<AppearanceService>) -> Router {
    Router::new()
        .route(
            "/api/v1/config/{registry}/{organization}/appearance",
            put(save),
        )
        .route(
            "/api/v1/config/{registry}/{organization}/appearance/publish",
            post(publish),
        )
        .route(
            "/api/v1/config/{registry}/{organization}/appearance/rollback",
            post(rollback),
        )
        .route(
            "/api/v1/config/{registry}/{organization}/appearance/preference",
            get(preference).put(save_preference),
        )
        .with_state(StateData { service })
}
pub fn public_router(service: Arc<AppearanceService>) -> Router {
    Router::new()
        .route(
            "/api/v1/config/{registry}/{organization}/appearance",
            get(current),
        )
        .with_state(StateData { service })
}
fn parse(b: ProfileInput) -> Result<(AppearanceProfile, Option<i64>), AppearanceError> {
    let color_mode = match b.color_mode.as_str() {
        "light" => ColorMode::Light,
        "dark" => ColorMode::Dark,
        "system" => ColorMode::System,
        _ => return Err(AppearanceError::Invalid),
    };
    let density = match b.density.as_str() {
        "comfortable" => Density::Comfortable,
        "compact" => Density::Compact,
        _ => return Err(AppearanceError::Invalid),
    };
    let version = b
        .expected_version
        .unwrap_or(0)
        .checked_add(1)
        .filter(|version| *version > 0)
        .ok_or(AppearanceError::Invalid)?;
    Ok((
        AppearanceProfile {
            name: b.name,
            accent: b.accent,
            on_accent: b.on_accent,
            logo_asset_ref: b.logo_asset_ref,
            color_mode,
            density,
            high_contrast: b.high_contrast,
            tokens: SemanticTokens {
                canvas: b.tokens.canvas,
                surface: b.tokens.surface,
                text: b.tokens.text,
                text_muted: b.tokens.text_muted,
                action: b.tokens.action,
                on_action: b.tokens.on_action,
                control_border: b.tokens.control_border,
                focus: b.tokens.focus,
            },
            version,
        },
        b.expected_version,
    ))
}

async fn current(
    State(s): State<StateData>,
    Path((r, o)): Path<(String, String)>,
) -> Result<Json<Option<ProfileBody>>, (StatusCode, Json<ApiError>)> {
    s.service
        .current(&scope(r, o))
        .await
        .map(|p| Json(p.map(Into::into)))
        .map_err(err)
}
async fn save(
    State(s): State<StateData>,
    Path((r, o)): Path<(String, String)>,
    ctx: Option<Extension<AccessContext>>,
    Json(b): Json<ProfileInput>,
) -> Result<StatusCode, (StatusCode, Json<ApiError>)> {
    let (p, v) = parse(b).map_err(err)?;
    s.service
        .save_draft(ctx.as_ref().map(|Extension(c)| c), &scope(r, o), p, v)
        .await
        .map_err(err)?;
    Ok(StatusCode::CREATED)
}
async fn publish(
    State(s): State<StateData>,
    Path((r, o)): Path<(String, String)>,
    ctx: Option<Extension<AccessContext>>,
    Json(b): Json<PublishInput>,
) -> Result<Json<ProfileBody>, (StatusCode, Json<ApiError>)> {
    s.service
        .publish(
            ctx.as_ref().map(|Extension(c)| c),
            &scope(r, o),
            b.expected_version,
        )
        .await
        .map(|p| Json(p.into()))
        .map_err(err)
}
async fn rollback(
    State(s): State<StateData>,
    Path((r, o)): Path<(String, String)>,
    ctx: Option<Extension<AccessContext>>,
    Json(b): Json<RollbackInput>,
) -> Result<Json<ProfileBody>, (StatusCode, Json<ApiError>)> {
    s.service
        .rollback(
            ctx.as_ref().map(|Extension(c)| c),
            &scope(r, o),
            b.target_version,
            b.expected_version,
        )
        .await
        .map(|p| Json(p.into()))
        .map_err(err)
}
async fn preference(
    State(s): State<StateData>,
    Path((r, o)): Path<(String, String)>,
    ctx: Option<Extension<AccessContext>>,
) -> Result<Json<Option<PreferenceBody>>, (StatusCode, Json<ApiError>)> {
    s.service
        .preference(ctx.as_ref().map(|Extension(c)| c), &scope(r, o))
        .await
        .map(|p| Json(p.map(Into::into)))
        .map_err(err)
}
async fn save_preference(
    State(s): State<StateData>,
    Path((r, o)): Path<(String, String)>,
    ctx: Option<Extension<AccessContext>>,
    Json(b): Json<PreferenceInput>,
) -> Result<StatusCode, (StatusCode, Json<ApiError>)> {
    let color_mode = match b.color_mode.as_deref() {
        None => None,
        Some("light") => Some(ColorMode::Light),
        Some("dark") => Some(ColorMode::Dark),
        Some("system") => Some(ColorMode::System),
        _ => return Err(err(AppearanceError::Invalid)),
    };
    let density = match b.density.as_deref() {
        None => None,
        Some("comfortable") => Some(Density::Comfortable),
        Some("compact") => Some(Density::Compact),
        _ => return Err(err(AppearanceError::Invalid)),
    };
    s.service
        .save_preference(
            ctx.as_ref().map(|Extension(c)| c),
            &scope(r, o),
            AppearancePreference {
                color_mode,
                density,
                high_contrast: b.high_contrast,
                locale_tag: b.locale_tag,
            },
        )
        .await
        .map_err(err)?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod version_tests {
    use super::*;
    #[test]
    fn invalid_draft_versions_are_rejected_before_persistence() {
        for version in [i64::MIN, -1, i64::MAX] {
            let input: ProfileInput = serde_json::from_value(serde_json::json!({
                "name":"Synthetic", "accent":"#000000", "on_accent":"#FFFFFF",
                "color_mode":"system", "density":"comfortable", "high_contrast":false,
                "expected_version":version,
                "tokens":{"canvas":"#FFFFFF","surface":"#FFFFFF","text":"#000000",
                    "text_muted":"#000000","action":"#000000","on_action":"#FFFFFF",
                    "control_border":"#000000","focus":"#000000"}
            }))
            .unwrap();
            assert!(matches!(parse(input), Err(AppearanceError::Invalid)));
        }
    }
}
