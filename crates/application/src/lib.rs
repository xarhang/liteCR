//! Use-case-facing ports. This crate intentionally has no web or database dependencies.

#![forbid(unsafe_code)]

pub mod access;
pub mod appearance;
pub mod country_profiles;
mod locale_tag;
pub mod localization;
pub mod matching;
pub mod migration;
pub mod registry;
pub mod review;
pub mod worker;
pub mod worker_runtime;

use async_trait::async_trait;
use litecr_domain::SchemaVersion;
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadinessSnapshot {
    pub database_reachable: bool,
    pub schema_version: Option<SchemaVersion>,
}

impl ReadinessSnapshot {
    pub const fn is_ready(self) -> bool {
        self.database_reachable
            && matches!(self.schema_version, Some(version) if version.is_compatible())
    }
}

#[derive(Debug, Error)]
pub enum ReadinessError {
    #[error("readiness probe failed")]
    ProbeFailed,
}

#[async_trait]
pub trait ReadinessProbe: Send + Sync + 'static {
    async fn snapshot(&self) -> Result<ReadinessSnapshot, ReadinessError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readiness_requires_database_and_compatible_schema() {
        assert!(
            ReadinessSnapshot {
                database_reachable: true,
                schema_version: Some(SchemaVersion::new(litecr_domain::SCHEMA_VERSION)),
            }
            .is_ready()
        );
        assert!(
            !ReadinessSnapshot {
                database_reachable: false,
                schema_version: Some(SchemaVersion::new(litecr_domain::SCHEMA_VERSION)),
            }
            .is_ready()
        );
        assert!(
            !ReadinessSnapshot {
                database_reachable: true,
                schema_version: None,
            }
            .is_ready()
        );
    }
}
