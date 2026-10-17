//! Versioned translation catalogs. Bundles are immutable after publication.

mod service;

pub use service::{
    CatalogEntry, CatalogError, CatalogRepository, CatalogService, DraftTranslation, Locale,
    MessageSource, PublishedBundle, ReviewDecision, TranslationState, fallback_value,
    plural_categories, valid_bundle, valid_plural_forms, valid_plural_values, valid_translation,
};
