//! Bounded, versioned country-specific form configuration.
mod service;
pub use service::{
    CountryProfile, CountryProfileCoverage, CountryProfileRepository, CountryProfileService,
    ProfileError,
};
