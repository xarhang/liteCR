use crate::access::{AccessContext, AccessDenied, Permission, ResourceScope, authorize};
use crate::locale_tag::canonical_bcp47_tag;
use async_trait::async_trait;
use std::sync::Arc;
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorMode {
    Light,
    Dark,
    System,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Density {
    Comfortable,
    Compact,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReleaseState {
    Draft,
    Published,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticTokens {
    pub canvas: String,
    pub surface: String,
    pub text: String,
    pub text_muted: String,
    pub action: String,
    pub on_action: String,
    pub control_border: String,
    pub focus: String,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppearanceProfile {
    pub name: String,
    pub accent: String,
    pub on_accent: String,
    pub logo_asset_ref: Option<String>,
    pub color_mode: ColorMode,
    pub density: Density,
    pub high_contrast: bool,
    pub tokens: SemanticTokens,
    pub version: i64,
}
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AppearancePreference {
    pub color_mode: Option<ColorMode>,
    pub density: Option<Density>,
    pub high_contrast: Option<bool>,
    pub locale_tag: Option<String>,
}
#[derive(Debug, Error)]
pub enum AppearanceError {
    #[error("appearance config is invalid")]
    Invalid,
    #[error("appearance access denied")]
    Denied(#[from] AccessDenied),
    #[error("appearance version conflict")]
    Conflict,
    #[error("appearance not found")]
    NotFound,
    #[error("appearance repository unavailable")]
    Unavailable,
}
#[async_trait]
pub trait AppearanceRepository: Send + Sync + 'static {
    async fn save_draft(
        &self,
        scope: &ResourceScope,
        profile: &AppearanceProfile,
        expected_version: Option<i64>,
        actor: &str,
    ) -> Result<(), AppearanceError>;
    async fn publish(
        &self,
        scope: &ResourceScope,
        expected_version: i64,
        actor: &str,
    ) -> Result<AppearanceProfile, AppearanceError>;
    async fn rollback(
        &self,
        scope: &ResourceScope,
        target_version: i64,
        expected_version: i64,
        actor: &str,
    ) -> Result<AppearanceProfile, AppearanceError>;
    async fn current(
        &self,
        scope: &ResourceScope,
    ) -> Result<Option<AppearanceProfile>, AppearanceError>;
    async fn save_preference(
        &self,
        scope: &ResourceScope,
        actor: &str,
        preference: &AppearancePreference,
    ) -> Result<(), AppearanceError>;
    async fn preference(
        &self,
        scope: &ResourceScope,
        actor: &str,
    ) -> Result<Option<AppearancePreference>, AppearanceError>;
}
pub struct AppearanceService {
    repository: Arc<dyn AppearanceRepository>,
}
impl AppearanceService {
    pub fn new(repository: Arc<dyn AppearanceRepository>) -> Self {
        Self { repository }
    }
    pub async fn save_draft(
        &self,
        context: Option<&AccessContext>,
        scope: &ResourceScope,
        profile: AppearanceProfile,
        expected_version: Option<i64>,
    ) -> Result<(), AppearanceError> {
        authorize(context, Permission::AppearanceManage, scope)?;
        if !valid(&profile) || expected_version.is_some_and(|v| v < 0) {
            return Err(AppearanceError::Invalid);
        }
        self.repository
            .save_draft(scope, &profile, expected_version, actor(context)?)
            .await
    }
    pub async fn publish(
        &self,
        context: Option<&AccessContext>,
        scope: &ResourceScope,
        expected_version: i64,
    ) -> Result<AppearanceProfile, AppearanceError> {
        authorize(context, Permission::AppearanceManage, scope)?;
        if expected_version < 0 {
            return Err(AppearanceError::Invalid);
        }
        self.repository
            .publish(scope, expected_version, actor(context)?)
            .await
    }
    pub async fn rollback(
        &self,
        context: Option<&AccessContext>,
        scope: &ResourceScope,
        target_version: i64,
        expected_version: i64,
    ) -> Result<AppearanceProfile, AppearanceError> {
        authorize(context, Permission::AppearanceManage, scope)?;
        if target_version < 1 || expected_version < 1 || target_version == expected_version {
            return Err(AppearanceError::Invalid);
        }
        self.repository
            .rollback(scope, target_version, expected_version, actor(context)?)
            .await
    }
    pub async fn current(
        &self,
        scope: &ResourceScope,
    ) -> Result<Option<AppearanceProfile>, AppearanceError> {
        self.repository.current(scope).await
    }
    pub async fn save_preference(
        &self,
        context: Option<&AccessContext>,
        scope: &ResourceScope,
        mut preference: AppearancePreference,
    ) -> Result<(), AppearanceError> {
        authorize(context, Permission::AppearancePreferenceManage, scope)?;
        if let Some(tag) = preference.locale_tag.as_deref() {
            preference.locale_tag = Some(canonical_bcp47_tag(tag).ok_or(AppearanceError::Invalid)?);
        }
        self.repository
            .save_preference(scope, actor(context)?, &preference)
            .await
    }
    pub async fn preference(
        &self,
        context: Option<&AccessContext>,
        scope: &ResourceScope,
    ) -> Result<Option<AppearancePreference>, AppearanceError> {
        authorize(context, Permission::AppearancePreferenceManage, scope)?;
        self.repository.preference(scope, actor(context)?).await
    }
}
fn actor(context: Option<&AccessContext>) -> Result<&str, AppearanceError> {
    context
        .map(|c| c.actor().id.as_str())
        .ok_or(AppearanceError::Denied(AccessDenied::Unauthenticated))
}
fn hex_color(value: &str) -> bool {
    value.len() == 7 && value.starts_with('#') && value[1..].bytes().all(|b| b.is_ascii_hexdigit())
}
fn contrast(a: &str, b: &str) -> f64 {
    let l = |s: &str| {
        let rgb = (1..7)
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap_or(0) as f64 / 255.0)
            .map(|c| {
                if c <= 0.04045 {
                    c / 12.92
                } else {
                    ((c + 0.055) / 1.055).powf(2.4)
                }
            })
            .collect::<Vec<_>>();
        0.2126 * rgb[0] + 0.7152 * rgb[1] + 0.0722 * rgb[2]
    };
    let (x, y) = (l(a), l(b));
    (x.max(y) + 0.05) / (x.min(y) + 0.05)
}
fn valid(p: &AppearanceProfile) -> bool {
    !p.name.trim().is_empty()
        && p.name.len() <= 96
        && !p.name.contains(['<', '>'])
        && !p.name.chars().any(char::is_control)
        && hex_color(&p.accent)
        && hex_color(&p.on_accent)
        && contrast(&p.accent, &p.on_accent) >= 4.5
        && p.logo_asset_ref.as_ref().is_none_or(|v| {
            !v.is_empty()
                && v.len() <= 128
                && v.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
        })
        && valid_tokens(&p.tokens)
        && p.version >= 1
}

fn valid_tokens(t: &SemanticTokens) -> bool {
    let colors = [
        &t.canvas,
        &t.surface,
        &t.text,
        &t.text_muted,
        &t.action,
        &t.on_action,
        &t.control_border,
        &t.focus,
    ];
    colors.iter().all(|color| hex_color(color))
        && contrast(&t.canvas, &t.text) >= 4.5
        && contrast(&t.surface, &t.text) >= 4.5
        && contrast(&t.canvas, &t.text_muted) >= 4.5
        && contrast(&t.action, &t.on_action) >= 4.5
        && contrast(&t.surface, &t.control_border) >= 3.0
        && contrast(&t.surface, &t.focus) >= 3.0
}
#[cfg(test)]
mod tests {
    use super::*;
    fn tokens() -> SemanticTokens {
        SemanticTokens {
            canvas: "#F8FAFC".into(),
            surface: "#FFFFFF".into(),
            text: "#0F172A".into(),
            text_muted: "#475569".into(),
            action: "#0F766E".into(),
            on_action: "#FFFFFF".into(),
            control_border: "#64748B".into(),
            focus: "#1D4ED8".into(),
        }
    }
    fn profile() -> AppearanceProfile {
        AppearanceProfile {
            name: "Demo".into(),
            accent: "#0F766E".into(),
            on_accent: "#FFFFFF".into(),
            logo_asset_ref: Some("logo-demo-1".into()),
            color_mode: ColorMode::System,
            density: Density::Comfortable,
            high_contrast: false,
            tokens: tokens(),
            version: 1,
        }
    }
    #[test]
    fn rejects_unbounded_or_unsafe_brand_data() {
        assert!(valid(&profile()));
        let mut p = profile();
        p.name = "<script>".into();
        assert!(!valid(&p));
        p.logo_asset_ref = Some("https://bad.example/logo.svg".into());
        assert!(!valid(&p));
    }
    #[test]
    fn rejects_inaccessible_accent_pair() {
        let mut p = profile();
        p.accent = "#EEEEEE".into();
        p.on_accent = "#FFFFFF".into();
        assert!(!valid(&p));
    }
    #[test]
    fn rejects_semantic_token_contrast_failures() {
        let mut p = profile();
        p.tokens.text = "#FFFFFF".into();
        assert!(!valid(&p));
    }
}
