//! PostgreSQL adapters and embedded schema migrations.

#![forbid(unsafe_code)]

pub mod access;
pub mod appearance;
pub mod country_profiles;
pub mod localization;
pub mod matching;
pub mod migration;
pub mod registry;
pub mod review;
pub mod worker;

use async_trait::async_trait;
use litecr_application::{ReadinessError, ReadinessProbe, ReadinessSnapshot};
use litecr_domain::SchemaVersion;
use sqlx::{PgPool, migrate::Migrator};
use thiserror::Error;

pub static MIGRATOR: Migrator = sqlx::migrate!("./migrations");

#[derive(Debug, Error)]
pub enum DatabaseError {
    #[error("database operation failed")]
    Operation(#[from] sqlx::Error),
    #[error("database does not meet the required local WAL durability policy")]
    DurabilityPolicy,
    #[error("database migration failed")]
    Migration(#[from] sqlx::migrate::MigrateError),
}

pub async fn connect(database_url: &str, max_connections: u32) -> Result<PgPool, DatabaseError> {
    let pool = pool_options(max_connections).connect(database_url).await?;
    validate_local_durability(&pool).await?;
    Ok(pool)
}

fn pool_options(max_connections: u32) -> sqlx::postgres::PgPoolOptions {
    sqlx::postgres::PgPoolOptions::new()
        .max_connections(max_connections)
        .acquire_timeout(std::time::Duration::from_secs(5))
        // PostgreSQL can acknowledge a commit before its local WAL flush when
        // this session setting is weakened. Establish the safer setting on
        // every connection, overriding role/database/startup defaults.
        .after_connect(|connection, _| {
            Box::pin(async move {
                sqlx::query("SET synchronous_commit = on")
                    .execute(connection)
                    .await?;
                Ok(())
            })
        })
}

async fn validate_local_durability(pool: &PgPool) -> Result<(), DatabaseError> {
    let (fsync, synchronous_commit): (String, String) =
        sqlx::query_as("SELECT current_setting('fsync'), current_setting('synchronous_commit')")
            .fetch_one(pool)
            .await?;
    if !local_durability_settings_are_safe(&fsync, &synchronous_commit) {
        return Err(DatabaseError::DurabilityPolicy);
    }
    Ok(())
}

/// A name-free snapshot of configured replication policy and visible streaming
/// state. Counts are operational hints only, not a durability/HA assertion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SynchronousReplicationStatus {
    pub policy_configured: bool,
    pub streaming_standbys: i64,
    pub synchronous_standbys: i64,
    pub quorum_candidates: i64,
    pub asynchronous_standbys: i64,
    pub potential_standbys: i64,
}

pub async fn synchronous_replication_status(
    pool: &PgPool,
) -> Result<SynchronousReplicationStatus, DatabaseError> {
    let (policy, streaming, synchronous, quorum, asynchronous, potential): (
        String,
        i64,
        i64,
        i64,
        i64,
        i64,
    ) = sqlx::query_as(
        "SELECT current_setting('synchronous_standby_names'), \
         count(*) FILTER (WHERE state = 'streaming'), \
         count(*) FILTER (WHERE state = 'streaming' AND sync_state = 'sync'), \
         count(*) FILTER (WHERE state = 'streaming' AND sync_state = 'quorum'), \
         count(*) FILTER (WHERE state = 'streaming' AND sync_state = 'async'), \
         count(*) FILTER (WHERE state = 'streaming' AND sync_state = 'potential') \
         FROM pg_stat_replication",
    )
    .fetch_one(pool)
    .await?;
    Ok(SynchronousReplicationStatus {
        policy_configured: synchronous_standby_names_is_configured(&policy),
        streaming_standbys: streaming,
        synchronous_standbys: synchronous,
        quorum_candidates: quorum,
        asynchronous_standbys: asynchronous,
        potential_standbys: potential,
    })
}

fn synchronous_standby_names_is_configured(setting: &str) -> bool {
    !setting.trim().is_empty()
}

pub async fn migrate(pool: &PgPool) -> Result<(), DatabaseError> {
    MIGRATOR.run(pool).await?;
    Ok(())
}

pub async fn schema_version(pool: &PgPool) -> Result<Option<SchemaVersion>, DatabaseError> {
    let version = sqlx::query_scalar::<_, i64>(
        "SELECT version FROM litecr.schema_meta WHERE singleton = TRUE",
    )
    .fetch_optional(pool)
    .await;
    let version = match version {
        Ok(version) => version,
        // Only a missing schema/table means liteCR has not been migrated.
        // Permission, connection, and other failures must remain errors.
        Err(sqlx::Error::Database(error))
            if schema_version_query_means_uninstalled(error.code().as_deref()) =>
        {
            return Ok(None);
        }
        Err(error) => return Err(DatabaseError::Operation(error)),
    };
    Ok(version.map(SchemaVersion::new))
}

fn schema_version_query_means_uninstalled(sqlstate: Option<&str>) -> bool {
    matches!(sqlstate, Some("3F000" | "42P01"))
}

pub struct PostgresReadinessProbe {
    pool: PgPool,
}

impl PostgresReadinessProbe {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl ReadinessProbe for PostgresReadinessProbe {
    async fn snapshot(&self) -> Result<ReadinessSnapshot, ReadinessError> {
        if sqlx::query("SELECT 1").execute(&self.pool).await.is_err() {
            return Ok(ReadinessSnapshot {
                database_reachable: false,
                schema_version: None,
            });
        }

        let schema_version = schema_version(&self.pool)
            .await
            .map_err(|_| ReadinessError::ProbeFailed)?;
        validate_local_durability(&self.pool)
            .await
            .map_err(|_| ReadinessError::ProbeFailed)?;
        Ok(ReadinessSnapshot {
            database_reachable: true,
            schema_version,
        })
    }
}

fn local_durability_settings_are_safe(fsync: &str, synchronous_commit: &str) -> bool {
    fsync == "on" && synchronous_commit == "on"
}

#[cfg(test)]
mod durability_tests {
    use super::*;

    #[test]
    fn schema_version_only_treats_missing_schema_or_table_as_uninstalled() {
        assert!(schema_version_query_means_uninstalled(Some("3F000")));
        assert!(schema_version_query_means_uninstalled(Some("42P01")));
        assert!(!schema_version_query_means_uninstalled(Some("42501")));
        assert!(!schema_version_query_means_uninstalled(Some("08006")));
        assert!(!schema_version_query_means_uninstalled(None));
    }

    #[test]
    fn local_durability_policy_rejects_weakened_settings() {
        assert!(local_durability_settings_are_safe("on", "on"));
        assert!(!local_durability_settings_are_safe("off", "on"));
        assert!(!local_durability_settings_are_safe("on", "off"));
        assert!(!local_durability_settings_are_safe("on", "remote_write"));
    }

    #[test]
    fn synchronous_standby_configuration_is_detected_without_exposing_names() {
        assert!(!synchronous_standby_names_is_configured(""));
        assert!(!synchronous_standby_names_is_configured("   "));
        assert!(synchronous_standby_names_is_configured(
            "FIRST 1 (private-node-name)"
        ));
    }

    #[tokio::test]
    async fn pool_connection_overrides_weakened_startup_commit_setting() {
        let Ok(database_url) = std::env::var("LITECR_TEST_DATABASE_URL") else {
            eprintln!("skipping PostgreSQL durability test: LITECR_TEST_DATABASE_URL is unset");
            return;
        };
        let options = database_url
            .parse::<sqlx::postgres::PgConnectOptions>()
            .expect("test database URL parses")
            .options([("synchronous_commit", "off")]);
        let pool = pool_options(1)
            .connect_with(options)
            .await
            .expect("test PostgreSQL accepts a pool with guarded sessions");
        validate_local_durability(&pool)
            .await
            .expect("pool enforces local WAL durability despite startup override");
        pool.close().await;
    }
}
