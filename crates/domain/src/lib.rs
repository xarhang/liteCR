//! Framework-independent domain foundation.

#![forbid(unsafe_code)]

pub mod registry;
pub mod review;
pub mod worker;

/// Version of the database schema expected by this release.
pub const SCHEMA_VERSION: i64 = 31;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SchemaVersion(i64);

impl SchemaVersion {
    pub const fn new(value: i64) -> Self {
        Self(value)
    }

    pub const fn value(self) -> i64 {
        self.0
    }

    pub const fn is_compatible(self) -> bool {
        self.0 == SCHEMA_VERSION
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_compatibility_requires_exact_version() {
        assert!(SchemaVersion::new(SCHEMA_VERSION).is_compatible());
        assert!(!SchemaVersion::new(0).is_compatible());
        assert!(!SchemaVersion::new(SCHEMA_VERSION + 1).is_compatible());
    }
}
