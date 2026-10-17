use crate::access::{AccessContext, AccessDenied, Permission, ResourceScope, authorize};
use crate::locale_tag::canonical_bcp47_tag;
use async_trait::async_trait;
use std::sync::Arc;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CountryProfile {
    pub country_code: String,
    pub default_locale: String,
    pub calendar: String,
    pub timezone: String,
    pub address_fields: Vec<String>,
    pub identifier_types: Vec<String>,
    pub phone_region: String,
    pub matching_policy_ref: Option<String>,
    pub version: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CountryProfileCoverage {
    pub country_code: String,
    pub latest_version: i64,
    pub active_version: Option<i64>,
}

#[derive(Debug, Error)]
pub enum ProfileError {
    #[error("country profile is invalid")]
    Invalid,
    #[error("country profile access denied")]
    Denied(#[from] AccessDenied),
    #[error("country profile version conflict")]
    Conflict,
    #[error("country profile not found")]
    NotFound,
    #[error("country profile repository unavailable")]
    Unavailable,
}

#[async_trait]
pub trait CountryProfileRepository: Send + Sync + 'static {
    async fn coverage(
        &self,
        scope: &ResourceScope,
    ) -> Result<Vec<CountryProfileCoverage>, ProfileError>;
    async fn save_draft(
        &self,
        scope: &ResourceScope,
        profile: &CountryProfile,
        expected_version: Option<i64>,
        actor: &str,
    ) -> Result<(), ProfileError>;
    async fn publish(
        &self,
        scope: &ResourceScope,
        country: &str,
        expected_version: i64,
        actor: &str,
    ) -> Result<CountryProfile, ProfileError>;
    async fn rollback(
        &self,
        scope: &ResourceScope,
        country: &str,
        target_version: i64,
        expected_version: i64,
        actor: &str,
    ) -> Result<CountryProfile, ProfileError>;
    async fn current(
        &self,
        scope: &ResourceScope,
        country: &str,
    ) -> Result<Option<CountryProfile>, ProfileError>;
}

pub struct CountryProfileService {
    repository: Arc<dyn CountryProfileRepository>,
}
impl CountryProfileService {
    pub fn new(repository: Arc<dyn CountryProfileRepository>) -> Self {
        Self { repository }
    }
    pub async fn coverage(
        &self,
        scope: &ResourceScope,
    ) -> Result<Vec<CountryProfileCoverage>, ProfileError> {
        self.repository.coverage(scope).await
    }
    pub async fn save_draft(
        &self,
        context: Option<&AccessContext>,
        scope: &ResourceScope,
        mut profile: CountryProfile,
        expected_version: Option<i64>,
    ) -> Result<(), ProfileError> {
        authorize(context, Permission::CountryProfileManage, scope)?;
        let Some(locale) = canonical_bcp47_tag(&profile.default_locale) else {
            return Err(ProfileError::Invalid);
        };
        profile.default_locale = locale;
        if !valid(&profile) || expected_version.is_some_and(|v| v < 0) {
            return Err(ProfileError::Invalid);
        }
        self.repository
            .save_draft(scope, &profile, expected_version, actor(context)?)
            .await
    }
    pub async fn publish(
        &self,
        context: Option<&AccessContext>,
        scope: &ResourceScope,
        country: &str,
        expected_version: i64,
    ) -> Result<CountryProfile, ProfileError> {
        authorize(context, Permission::CountryProfileManage, scope)?;
        if !country_code(country) || expected_version < 1 {
            return Err(ProfileError::Invalid);
        }
        self.repository
            .publish(scope, country, expected_version, actor(context)?)
            .await
    }
    pub async fn rollback(
        &self,
        context: Option<&AccessContext>,
        scope: &ResourceScope,
        country: &str,
        target_version: i64,
        expected_version: i64,
    ) -> Result<CountryProfile, ProfileError> {
        authorize(context, Permission::CountryProfileManage, scope)?;
        if !country_code(country)
            || target_version < 1
            || expected_version < 1
            || target_version == expected_version
        {
            return Err(ProfileError::Invalid);
        }
        self.repository
            .rollback(
                scope,
                country,
                target_version,
                expected_version,
                actor(context)?,
            )
            .await
    }
    pub async fn current(
        &self,
        scope: &ResourceScope,
        country: &str,
    ) -> Result<Option<CountryProfile>, ProfileError> {
        if !country_code(country) {
            return Err(ProfileError::Invalid);
        }
        self.repository.current(scope, country).await
    }
}
fn actor(context: Option<&AccessContext>) -> Result<&str, ProfileError> {
    context
        .map(|c| c.actor().id.as_str())
        .ok_or(ProfileError::Denied(AccessDenied::Unauthenticated))
}
fn country_code(value: &str) -> bool {
    value.len() == 2 && value.bytes().all(|b| b.is_ascii_uppercase())
}
fn bounded_slug(value: &str, max: usize) -> bool {
    !value.is_empty()
        && value.len() <= max
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}
fn valid(p: &CountryProfile) -> bool {
    country_code(&p.country_code)
        && valid_locale(&p.default_locale)
        && bounded_slug(&p.calendar, 32)
        && p.timezone.len() <= 64
        && p.timezone.contains('/')
        && p.timezone
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'/' || b == b'_' || b == b'-' || b == b'+')
        && p.address_fields.len() <= 24
        && p.address_fields.iter().all(|v| bounded_slug(v, 48))
        && p.identifier_types.len() <= 32
        && p.identifier_types.iter().all(|v| bounded_slug(v, 64))
        && country_code(&p.phone_region)
        && p.matching_policy_ref.is_none()
        && p.version >= 1
}
fn valid_locale(value: &str) -> bool {
    canonical_bcp47_tag(value).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn profile() -> CountryProfile {
        CountryProfile {
            country_code: "LA".into(),
            default_locale: "lo-LA".into(),
            calendar: "gregory".into(),
            timezone: "Asia/Vientiane".into(),
            address_fields: vec!["province".into(), "district".into()],
            identifier_types: vec!["national-id".into()],
            phone_region: "LA".into(),
            matching_policy_ref: None,
            version: 1,
        }
    }
    #[test]
    fn profile_is_bounded_and_cannot_switch_match_policy() {
        assert!(valid(&profile()));
        let mut p = profile();
        p.address_fields = vec!["x".into(); 25];
        assert!(!valid(&p));
        let mut p = profile();
        p.matching_policy_ref = Some("new-policy".into());
        assert!(!valid(&p));
    }
    #[test]
    fn accepts_country_locale_calendar_and_timezone_independently() {
        assert!(valid(&profile()));
        let mut p = profile();
        p.default_locale = "hi-IN".into();
        p.calendar = "buddhist".into();
        p.timezone = "Asia/Kolkata".into();
        assert!(valid(&p));
    }

    #[test]
    fn country_profile_default_locale_uses_bcp47_validation() {
        let mut p = profile();
        p.default_locale = "zh-Hant-TW-u-nu-hanidec".into();
        assert!(valid(&p));

        for locale in ["lo_LA", "en--US", "en-u", "abcdefghi"] {
            p.default_locale = locale.into();
            assert!(!valid(&p), "accepted malformed locale {locale}");
        }
    }
}
