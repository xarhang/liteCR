//! Validated design tokens and audited, versioned brand releases.
mod service;
pub use service::{
    AppearanceError, AppearancePreference, AppearanceProfile, AppearanceRepository,
    AppearanceService, ColorMode, Density, ReleaseState, SemanticTokens,
};
