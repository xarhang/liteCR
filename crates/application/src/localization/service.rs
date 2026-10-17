use std::collections::{BTreeMap, BTreeSet};

use async_trait::async_trait;
use icu_locid::Locale as IcuLocale;
use icu_plurals::{PluralCategory, PluralRuleType, PluralRules};
use thiserror::Error;

use crate::access::{AccessContext, AccessDenied, Permission, ResourceScope, authorize};
use crate::locale_tag::canonical_bcp47_tag;

const MAX_NAMESPACE: usize = 96;
const MAX_KEY: usize = 192;
const MAX_TEXT: usize = 8_192;
const MAX_ENTRIES: usize = 10_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Locale {
    pub tag: String,
    pub direction: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageSource {
    pub namespace: String,
    pub key: String,
    pub source_text: String,
    pub source_revision: i64,
    pub placeholders: BTreeSet<String>,
    pub plural: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogEntry {
    pub source: MessageSource,
    pub draft: Option<DraftTranslation>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranslationState {
    Draft,
    InReview,
    Approved,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftTranslation {
    pub locale: String,
    pub namespace: String,
    pub key: String,
    pub value: String,
    /// All locale-specific CLDR cardinal forms for plural messages. `value` mirrors `other`.
    pub plural_forms: Option<BTreeMap<String, String>>,
    pub source_revision: i64,
    pub state: TranslationState,
    pub stale: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishedBundle {
    pub locale: String,
    pub namespace: String,
    pub revision: i64,
    pub entries: BTreeMap<String, String>,
    pub fallback_locale: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewDecision {
    Approve,
    RequestChanges,
}

#[derive(Debug, Error)]
pub enum CatalogError {
    #[error("catalog input is invalid")]
    Invalid,
    #[error("catalog access denied")]
    Denied(#[from] AccessDenied),
    #[error("catalog version conflict")]
    Conflict,
    #[error("catalog item not found")]
    NotFound,
    #[error("catalog repository unavailable")]
    Unavailable,
}

#[async_trait]
pub trait CatalogRepository: Send + Sync + 'static {
    async fn add_locale(
        &self,
        scope: &ResourceScope,
        locale: &Locale,
        actor: &str,
    ) -> Result<(), CatalogError>;
    /// Resolve an equivalent BCP 47 tag to its existing storage key. More than one
    /// legacy case-variant is a conflict and must be repaired explicitly.
    async fn resolve_locale_tag(
        &self,
        scope: &ResourceScope,
        canonical_tag: &str,
    ) -> Result<Option<String>, CatalogError>;
    async fn list_locales(&self, scope: &ResourceScope) -> Result<Vec<Locale>, CatalogError>;
    /// List locales with an active UI bundle. This exposes publication metadata only.
    async fn list_published_ui_locales(
        &self,
        scope: &ResourceScope,
    ) -> Result<Vec<Locale>, CatalogError>;
    async fn set_source(
        &self,
        scope: &ResourceScope,
        source: &MessageSource,
        actor: &str,
    ) -> Result<(), CatalogError>;
    async fn save_draft(
        &self,
        scope: &ResourceScope,
        draft: &DraftTranslation,
        actor: &str,
    ) -> Result<(), CatalogError>;
    async fn load_draft(
        &self,
        scope: &ResourceScope,
        locale: &str,
        namespace: &str,
        key: &str,
    ) -> Result<Option<DraftTranslation>, CatalogError>;
    async fn load_source(
        &self,
        scope: &ResourceScope,
        namespace: &str,
        key: &str,
    ) -> Result<Option<MessageSource>, CatalogError>;
    async fn list_entries(
        &self,
        scope: &ResourceScope,
        locale: &str,
        namespace: &str,
    ) -> Result<Vec<CatalogEntry>, CatalogError>;
    async fn review(
        &self,
        scope: &ResourceScope,
        locale: &str,
        namespace: &str,
        key: &str,
        decision: ReviewDecision,
        actor: &str,
    ) -> Result<(), CatalogError>;
    async fn publish(
        &self,
        scope: &ResourceScope,
        locale: &str,
        namespace: &str,
        expected_revision: Option<i64>,
        actor: &str,
    ) -> Result<PublishedBundle, CatalogError>;
    async fn rollback(
        &self,
        scope: &ResourceScope,
        locale: &str,
        namespace: &str,
        target_revision: i64,
        expected_revision: Option<i64>,
        actor: &str,
    ) -> Result<PublishedBundle, CatalogError>;
    async fn load_bundle(
        &self,
        scope: &ResourceScope,
        locale: &str,
        namespace: &str,
    ) -> Result<Option<PublishedBundle>, CatalogError>;
}

pub struct CatalogService {
    repository: std::sync::Arc<dyn CatalogRepository>,
}

impl CatalogService {
    pub fn new(repository: std::sync::Arc<dyn CatalogRepository>) -> Self {
        Self { repository }
    }

    pub async fn add_locale(
        &self,
        context: Option<&AccessContext>,
        scope: &ResourceScope,
        locale: Locale,
    ) -> Result<(), CatalogError> {
        authorize(context, Permission::TranslationDraftWrite, scope)?;
        let Some(tag) = canonical_bcp47_tag(&locale.tag) else {
            return Err(CatalogError::Invalid);
        };
        if !matches!(locale.direction.as_str(), "ltr" | "rtl") {
            return Err(CatalogError::Invalid);
        }
        let locale = Locale { tag, ..locale };
        self.repository
            .add_locale(scope, &locale, actor(context)?)
            .await
    }

    pub async fn locales(
        &self,
        context: Option<&AccessContext>,
        scope: &ResourceScope,
    ) -> Result<Vec<Locale>, CatalogError> {
        authorize(context, Permission::TranslationDraftRead, scope)?;
        self.repository.list_locales(scope).await
    }

    /// Public shell discovery contains only enabled locales with an active UI bundle.
    pub async fn published_ui_locales(
        &self,
        scope: &ResourceScope,
    ) -> Result<Vec<Locale>, CatalogError> {
        self.repository.list_published_ui_locales(scope).await
    }

    pub async fn draft(
        &self,
        context: Option<&AccessContext>,
        scope: &ResourceScope,
        locale: &str,
        namespace: &str,
        key: &str,
    ) -> Result<Option<DraftTranslation>, CatalogError> {
        authorize(context, Permission::TranslationDraftRead, scope)?;
        if !valid_locale(locale) || !valid_key(namespace, MAX_NAMESPACE) || !valid_key(key, MAX_KEY)
        {
            return Err(CatalogError::Invalid);
        }
        let locale = self.storage_locale(scope, locale).await?;
        self.repository
            .load_draft(scope, &locale, namespace, key)
            .await
    }

    pub async fn entries(
        &self,
        context: Option<&AccessContext>,
        scope: &ResourceScope,
        locale: &str,
        namespace: &str,
    ) -> Result<Vec<CatalogEntry>, CatalogError> {
        authorize(context, Permission::TranslationDraftRead, scope)?;
        if !valid_locale(locale) || !valid_key(namespace, MAX_NAMESPACE) {
            return Err(CatalogError::Invalid);
        }
        let locale = self.storage_locale(scope, locale).await?;
        self.repository
            .list_entries(scope, &locale, namespace)
            .await
    }

    pub async fn set_source(
        &self,
        context: Option<&AccessContext>,
        scope: &ResourceScope,
        source: MessageSource,
    ) -> Result<(), CatalogError> {
        authorize(context, Permission::TranslationDraftWrite, scope)?;
        if !valid_key(&source.namespace, MAX_NAMESPACE)
            || !valid_key(&source.key, MAX_KEY)
            || source.source_text.trim().is_empty()
            || source.source_text.len() > MAX_TEXT
            || source.source_revision < 1
            || placeholders(&source.source_text) != source.placeholders
        {
            return Err(CatalogError::Invalid);
        }
        self.repository
            .set_source(scope, &source, actor(context)?)
            .await
    }

    pub async fn save_draft(
        &self,
        context: Option<&AccessContext>,
        scope: &ResourceScope,
        draft: DraftTranslation,
    ) -> Result<(), CatalogError> {
        authorize(context, Permission::TranslationDraftWrite, scope)?;
        if !valid_locale(&draft.locale)
            || !valid_key(&draft.namespace, MAX_NAMESPACE)
            || !valid_key(&draft.key, MAX_KEY)
            || draft.value.len() > MAX_TEXT
            || draft.value.trim().is_empty()
            || draft.source_revision < 1
            || draft.value.contains(['<', '>'])
        {
            return Err(CatalogError::Invalid);
        }
        let locale = self.storage_locale(scope, &draft.locale).await?;
        let draft = DraftTranslation { locale, ..draft };
        let source = self
            .repository
            .load_source(scope, &draft.namespace, &draft.key)
            .await?
            .ok_or(CatalogError::NotFound)?;
        if source.source_revision != draft.source_revision || !valid_translation(&source, &draft) {
            return Err(CatalogError::Conflict);
        }
        self.repository
            .save_draft(
                scope,
                &DraftTranslation {
                    state: TranslationState::Draft,
                    stale: false,
                    ..draft
                },
                actor(context)?,
            )
            .await
    }

    pub async fn review(
        &self,
        context: Option<&AccessContext>,
        scope: &ResourceScope,
        locale: &str,
        namespace: &str,
        key: &str,
        decision: ReviewDecision,
    ) -> Result<(), CatalogError> {
        authorize(context, Permission::TranslationReview, scope)?;
        if !valid_locale(locale) || !valid_key(namespace, MAX_NAMESPACE) || !valid_key(key, MAX_KEY)
        {
            return Err(CatalogError::Invalid);
        }
        let locale = self.storage_locale(scope, locale).await?;
        self.repository
            .review(scope, &locale, namespace, key, decision, actor(context)?)
            .await
    }

    pub async fn publish(
        &self,
        context: Option<&AccessContext>,
        scope: &ResourceScope,
        locale: &str,
        namespace: &str,
        expected_revision: Option<i64>,
    ) -> Result<PublishedBundle, CatalogError> {
        authorize(context, Permission::TranslationPublish, scope)?;
        if !valid_locale(locale)
            || !valid_key(namespace, MAX_NAMESPACE)
            || expected_revision.is_some_and(|revision| revision < 0)
        {
            return Err(CatalogError::Invalid);
        }
        let locale = self.storage_locale(scope, locale).await?;
        self.repository
            .publish(
                scope,
                &locale,
                namespace,
                expected_revision,
                actor(context)?,
            )
            .await
    }

    pub async fn rollback(
        &self,
        context: Option<&AccessContext>,
        scope: &ResourceScope,
        locale: &str,
        namespace: &str,
        target_revision: i64,
        expected_revision: Option<i64>,
    ) -> Result<PublishedBundle, CatalogError> {
        authorize(context, Permission::TranslationPublish, scope)?;
        if !valid_locale(locale)
            || !valid_key(namespace, MAX_NAMESPACE)
            || target_revision < 1
            || expected_revision.is_some_and(|revision| revision < 0)
        {
            return Err(CatalogError::Invalid);
        }
        let locale = self.storage_locale(scope, locale).await?;
        self.repository
            .rollback(
                scope,
                &locale,
                namespace,
                target_revision,
                expected_revision,
                actor(context)?,
            )
            .await
    }

    pub async fn public_bundle(
        &self,
        scope: &ResourceScope,
        locale: &str,
        namespace: &str,
    ) -> Result<Option<PublishedBundle>, CatalogError> {
        if !valid_locale(locale) || !valid_key(namespace, MAX_NAMESPACE) {
            return Err(CatalogError::Invalid);
        }
        let locale = self.storage_locale(scope, locale).await?;
        self.repository.load_bundle(scope, &locale, namespace).await
    }

    async fn storage_locale(
        &self,
        scope: &ResourceScope,
        requested: &str,
    ) -> Result<String, CatalogError> {
        let canonical = canonical_bcp47_tag(requested).ok_or(CatalogError::Invalid)?;
        Ok(self
            .repository
            .resolve_locale_tag(scope, &canonical)
            .await?
            .unwrap_or(canonical))
    }
}

pub fn fallback_value(
    bundle: &PublishedBundle,
    source: &BTreeMap<String, String>,
    key: &str,
) -> Option<String> {
    bundle.entries.get(key).or_else(|| source.get(key)).cloned()
}

pub fn valid_bundle(
    locale: &str,
    entries: &BTreeMap<String, String>,
    sources: &BTreeMap<String, MessageSource>,
) -> bool {
    valid_locale(locale)
        && !entries.is_empty()
        && entries.len() <= MAX_ENTRIES
        && entries.iter().all(|(entry, value)| {
            if let Some(source) = sources.get(entry) {
                !source.plural && valid_text(value) && placeholders(value) == source.placeholders
            } else {
                sources.iter().any(|(key, source)| {
                    source.plural
                        && entry
                            .strip_prefix(&format!("{key}_"))
                            .is_some_and(|category| plural_categories(locale).contains(&category))
                        && valid_text(value)
                        && placeholders(value) == source.placeholders
                })
            }
        })
        && sources
            .iter()
            .filter(|(_, source)| source.plural)
            .all(|(key, _)| {
                let forms = entries
                    .keys()
                    .filter_map(|entry| entry.strip_prefix(&format!("{key}_")).map(str::to_owned))
                    .collect::<BTreeSet<_>>();
                forms.is_empty()
                    || forms
                        == plural_categories(locale)
                            .into_iter()
                            .map(str::to_owned)
                            .collect()
            })
}

pub fn valid_plural_forms(locale: &str, forms: &BTreeMap<String, String>) -> bool {
    let categories = plural_categories(locale);
    !categories.is_empty()
        && forms.len() == categories.len()
        && categories
            .iter()
            .all(|category| forms.contains_key(*category))
        && forms
            .keys()
            .all(|category| categories.contains(&category.as_str()))
        && forms.values().all(|value| valid_text(value))
}

pub fn valid_translation(source: &MessageSource, draft: &DraftTranslation) -> bool {
    match (source.plural, draft.plural_forms.as_ref()) {
        (false, None) => {
            valid_text(&draft.value) && placeholders(&draft.value) == source.placeholders
        }
        (true, Some(forms)) => {
            draft.value == forms.get("other").map(String::as_str).unwrap_or("")
                && valid_plural_values(source, Some(forms))
                && valid_plural_forms(&draft.locale, forms)
        }
        _ => false,
    }
}

pub fn valid_plural_values(
    source: &MessageSource,
    forms: Option<&BTreeMap<String, String>>,
) -> bool {
    forms.is_some_and(|forms| {
        forms
            .values()
            .all(|value| valid_text(value) && placeholders(value) == source.placeholders)
    })
}

fn valid_text(value: &str) -> bool {
    !value.trim().is_empty()
        && value.len() <= MAX_TEXT
        && !value.contains(['<', '>'])
        && !value.chars().any(char::is_control)
}

/// Returns CLDR-backed cardinal plural categories for a well-formed locale tag.
/// This catalog validation is not a substitute for a locale's qualified translation review.
pub fn plural_categories(locale: &str) -> Vec<&'static str> {
    if !valid_locale(locale) {
        return Vec::new();
    }
    let Ok(locale) = locale.parse::<IcuLocale>() else {
        return Vec::new();
    };
    let data_locale = locale.into();
    let Ok(rules) = PluralRules::try_new(&data_locale, PluralRuleType::Cardinal) else {
        return Vec::new();
    };
    rules
        .categories()
        .map(|category| match category {
            PluralCategory::Zero => "zero",
            PluralCategory::One => "one",
            PluralCategory::Two => "two",
            PluralCategory::Few => "few",
            PluralCategory::Many => "many",
            PluralCategory::Other => "other",
        })
        .collect()
}

fn actor(context: Option<&AccessContext>) -> Result<&str, CatalogError> {
    context
        .map(|context| context.actor().id.as_str())
        .ok_or(CatalogError::Denied(AccessDenied::Unauthenticated))
}
fn valid_locale(value: &str) -> bool {
    canonical_bcp47_tag(value).is_some()
}
fn valid_key(value: &str, max: usize) -> bool {
    !value.is_empty()
        && value.len() <= max
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}
fn placeholders(value: &str) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    let mut rest = value;
    while let Some(start) = rest.find("{{") {
        rest = &rest[start + 2..];
        let Some(end) = rest.find("}}") else {
            return BTreeSet::new();
        };
        let name = &rest[..end];
        if name.is_empty()
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        {
            return BTreeSet::new();
        }
        found.insert(name.to_owned());
        rest = &rest[end + 2..];
    }
    if rest.contains("}}") {
        return BTreeSet::new();
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn placeholders_are_matched_and_markup_is_not_a_translation() {
        assert_eq!(
            placeholders("Hello {{name}}"),
            BTreeSet::from(["name".to_owned()])
        );
        assert!(placeholders("Hello {{name").is_empty());
        let source = MessageSource {
            namespace: "common".into(),
            key: "hello".into(),
            source_text: "Hello {{name}}".into(),
            source_revision: 1,
            placeholders: BTreeSet::from(["name".into()]),
            plural: false,
        };
        assert!(valid_bundle(
            "lo",
            &BTreeMap::from([("hello".into(), "Sabaidee {{name}}".into())]),
            &BTreeMap::from([("hello".into(), source.clone())])
        ));
        assert!(!valid_bundle(
            "lo",
            &BTreeMap::from([("hello".into(), "<b>{{name}}</b>".into())]),
            &BTreeMap::from([("hello".into(), source)])
        ));
    }

    #[test]
    fn locale_plural_categories_and_placeholders_are_checked_for_every_form() {
        let source = MessageSource {
            namespace: "common".into(),
            key: "items".into(),
            source_text: "{{count}} item(s) for {{name}}".into(),
            source_revision: 1,
            placeholders: BTreeSet::from(["count".into(), "name".into()]),
            plural: true,
        };
        let forms = BTreeMap::from([
            ("one".into(), "{{count}} item for {{name}}".into()),
            ("other".into(), "{{count}} items for {{name}}".into()),
        ]);
        assert!(valid_plural_forms("en", &forms));
        assert!(valid_plural_values(&source, Some(&forms)));
        assert!(valid_translation(
            &source,
            &DraftTranslation {
                locale: "en".into(),
                namespace: "common".into(),
                key: "items".into(),
                value: forms["other"].clone(),
                plural_forms: Some(forms.clone()),
                source_revision: 1,
                state: TranslationState::Draft,
                stale: false
            }
        ));
        assert!(!valid_plural_forms("ja", &forms));
        let lao_forms = BTreeMap::from([("other".into(), "{{count}} item(s) for {{name}}".into())]);
        assert!(valid_plural_forms("lo-LA", &lao_forms));
        assert!(valid_bundle(
            "lo-LA",
            &BTreeMap::from([("items_other".into(), lao_forms["other"].clone())]),
            &BTreeMap::from([("items".into(), source.clone())])
        ));
        let missing = BTreeMap::from([("other".into(), "items".into())]);
        assert!(!valid_plural_values(&source, Some(&missing)));
        let mut arabic = BTreeMap::new();
        for category in plural_categories("ar") {
            arabic.insert(category.into(), "{{count}} items for {{name}}".into());
        }
        assert!(valid_plural_forms("ar", &arabic));
        assert!(!valid_plural_forms("ar", &forms));
        arabic.insert("bogus".into(), "x".into());
        assert!(!valid_plural_forms("ar", &arabic));
    }

    #[test]
    fn cardinal_categories_come_from_cldr_for_selected_locales() {
        for locale in ["lo", "lo-LA", "th", "th-TH", "vi", "vi-VN"] {
            assert_eq!(plural_categories(locale), vec!["other"], "{locale}");
        }
        assert_eq!(plural_categories("hi-IN"), vec!["one", "other"]);
        assert_eq!(
            plural_categories("ar"),
            vec!["zero", "one", "two", "few", "many", "other"]
        );
        assert_eq!(plural_categories("ru"), vec!["one", "few", "many", "other"]);
        assert_eq!(plural_categories("pl"), vec!["one", "few", "many", "other"]);
    }

    #[test]
    fn locale_validation_uses_well_formed_bcp47_tags() {
        for locale in ["lo", "en-US", "hi-IN", "zh-Hant-TW-u-nu-hanidec"] {
            assert!(valid_locale(locale), "expected valid BCP 47 tag: {locale}");
        }
        for locale in ["", "en_US", "en--US", "-en", "en-@", "abcdefghi"] {
            assert!(
                !valid_locale(locale),
                "expected invalid BCP 47 tag: {locale}"
            );
            assert!(plural_categories(locale).is_empty());
        }
    }

    #[test]
    fn locale_tags_are_canonicalized_without_alias_migration() {
        assert_eq!(canonical_bcp47_tag("en-us").as_deref(), Some("en-US"));
        assert_eq!(
            canonical_bcp47_tag("zh-hant-tw-u-nu-hanidec").as_deref(),
            Some("zh-Hant-TW-u-nu-hanidec")
        );
        // Keep this lexical canonicalizer from silently applying CLDR alias migrations.
        assert_eq!(canonical_bcp47_tag("iw").as_deref(), Some("iw"));
    }
}
