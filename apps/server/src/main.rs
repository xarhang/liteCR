use std::{collections::HashMap, future::Future, net::SocketAddr, sync::Arc};

use axum::{
    body::Body,
    http::{Method, StatusCode, Uri, header},
    response::{IntoResponse, Response},
};
use clap::{Parser, Subcommand};
use litecr_api::{
    access::{AccessApiConfig, AccessEnvironment, protected_routes, router as access_router},
    router,
};
use litecr_application::{
    access::{AccessService, AccessServiceConfig},
    appearance::AppearanceService,
    country_profiles::CountryProfileService,
    localization::CatalogService,
    matching::MatchingService,
    migration::MigrationPackagePolicy,
    migration::MigrationService,
    registry::RegistryService,
    review::ReviewService,
};
use litecr_domain::{SCHEMA_VERSION, SchemaVersion};
use litecr_infrastructure::{
    PostgresReadinessProbe,
    access::{
        OidcProviderClient, OidcProviderConfig, PostgresAccessAuthenticator,
        PostgresAccessDirectory, PostgresAccessSecurityAudit, PostgresAccessSessionRepository,
        PostgresOidcFlowStore,
    },
    appearance::PostgresAppearanceRepository,
    connect,
    country_profiles::PostgresCountryProfileRepository,
    localization::PostgresCatalogRepository,
    matching::PostgresMatchingStore,
    migrate,
    migration::PostgresMigrationExecutor,
    registry::PostgresRegistryStore,
    review::PostgresReviewStore,
    schema_version, synchronous_replication_status,
};
use sqlx::PgPool;
use tracing::{error, info};
use tracing_subscriber::EnvFilter;

mod embedded_web {
    include!(concat!(env!("OUT_DIR"), "/embedded_web.rs"));
}

mod migration_cli;
#[cfg(windows)]
mod windows_service;
mod worker_cli;

#[derive(Debug, Parser)]
#[command(name = "litecr", version, about = "liteCR foundation service")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run the HTTP service.
    Serve,
    /// Apply embedded database migrations.
    Migrate,
    /// Check database connectivity and schema compatibility.
    Doctor,
    /// Run the optional advisory matching consumer with its dedicated database role.
    Worker,
    /// Run the HTTP service under the Windows Service Control Manager.
    ServiceApi,
    /// Run the advisory matching consumer under the Windows Service Control Manager.
    ServiceWorker,
    /// Maintain versioned matching indexes.
    Matching {
        #[command(subcommand)]
        command: MatchingCommand,
    },
    /// Print build version.
    Version {
        /// Print stable machine-readable release metadata.
        #[arg(long)]
        json: bool,
    },
    /// Validate, apply, inspect, or safely reverse an imported migration.
    Migration {
        #[command(subcommand)]
        command: Box<migration_cli::MigrationCommand>,
    },
}

#[derive(Debug, Subcommand)]
enum MatchingCommand {
    /// Reindex older accepted source versions in bounded, repeatable batches.
    Reindex {
        /// Maximum source versions to index in one transaction (1–1000).
        #[arg(long, default_value_t = 500)]
        batch_size: usize,
        /// Restrict the backfill to one registry.
        #[arg(long)]
        registry: Option<String>,
        /// Restrict the backfill to one organization.
        #[arg(long)]
        organization: Option<String>,
    },
}

#[derive(Clone)]
struct DatabaseConfig {
    database_url: String,
    max_connections: u32,
}

impl DatabaseConfig {
    fn from_env() -> Result<Self, &'static str> {
        Self::from_env_named("LITECR_DATABASE_URL")
    }

    fn migration_from_env() -> Result<Self, &'static str> {
        Self::from_env_named("LITECR_MIGRATION_DATABASE_URL")
    }

    fn from_env_named(name: &'static str) -> Result<Self, &'static str> {
        let database_url = std::env::var(name).map_err(|_| match name {
            "LITECR_MIGRATION_DATABASE_URL" => {
                "LITECR_MIGRATION_DATABASE_URL is required for migrate"
            }
            _ => "LITECR_DATABASE_URL is required",
        })?;
        let max_connections =
            std::env::var("LITECR_DATABASE_MAX_CONNECTIONS").unwrap_or_else(|_| "10".to_owned());
        Self::parse(database_url, &max_connections)
    }

    fn parse(database_url: String, max_connections: &str) -> Result<Self, &'static str> {
        if !database_url.starts_with("postgres://") && !database_url.starts_with("postgresql://") {
            return Err("LITECR_DATABASE_URL must use the postgres or postgresql scheme");
        }

        let max_connections = max_connections
            .parse::<u32>()
            .ok()
            .filter(|value| (1..=100).contains(value))
            .ok_or("LITECR_DATABASE_MAX_CONNECTIONS must be between 1 and 100")?;

        Ok(Self {
            database_url,
            max_connections,
        })
    }
}

fn configured_bind() -> Result<SocketAddr, &'static str> {
    let bind = std::env::var("LITECR_BIND")
        .unwrap_or_else(|_| "127.0.0.1:8080".to_owned())
        .parse::<SocketAddr>()
        .map_err(|_| "LITECR_BIND must be a valid socket address")?;
    let environment = std::env::var("LITECR_ENV").unwrap_or_else(|_| "production".to_owned());
    if environment == "production" && !bind.ip().is_loopback() {
        return Err("production LITECR_BIND must be loopback; use a local HTTPS gateway");
    }
    Ok(bind)
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .compact()
        .init();
}

async fn open_database(config: &DatabaseConfig) -> Result<PgPool, String> {
    connect(&config.database_url, config.max_connections)
        .await
        .map_err(|_| "database connection failed; verify service and credentials".to_owned())
}

async fn require_compatible_schema(pool: &PgPool) -> Result<SchemaVersion, String> {
    let version = schema_version(pool).await.map_err(|_| {
        "could not query liteCR schema metadata; verify database permissions and health".to_owned()
    })?;
    validate_schema_version(version)
}

fn validate_schema_version(version: Option<SchemaVersion>) -> Result<SchemaVersion, String> {
    match version {
        Some(version) if version.is_compatible() => Ok(version),
        Some(version) if version.value() < SCHEMA_VERSION => Err(format!(
            "database schema {} is older than application schema {}; run litecr migrate",
            version.value(),
            SCHEMA_VERSION
        )),
        Some(version) => Err(format!(
            "database schema {} is newer than application schema {}; use a compatible application release",
            version.value(),
            SCHEMA_VERSION
        )),
        None => Err("liteCR schema is not installed; run litecr migrate".to_owned()),
    }
}

fn doctor_replication_summary(
    status: litecr_infrastructure::SynchronousReplicationStatus,
) -> String {
    let observed = format!(
        "observed streaming standbys={} (sync={}, quorum_candidates={}, async={}, potential={})",
        status.streaming_standbys,
        status.synchronous_standbys,
        status.quorum_candidates,
        status.asynchronous_standbys,
        status.potential_standbys,
    );
    if status.policy_configured {
        format!(
            "replication: synchronous standby policy configured; {observed}; quorum sufficiency, failure-domain independence, fencing, and RPO/RTO are not verified"
        )
    } else {
        format!(
            "replication: no synchronous standby policy configured; commits rely on local WAL flush only; {observed}"
        )
    }
}

fn doctor_matching_index_summary(pending: i64) -> String {
    if pending == 0 {
        "matching index: current".to_owned()
    } else {
        format!(
            "matching index: {pending} accepted source version(s) need indexing; run `litecr matching reindex`"
        )
    }
}

async fn compose_access(
    pool: PgPool,
    bind: SocketAddr,
) -> Result<(axum::Router, Arc<AccessService>, AccessApiConfig), String> {
    let mode = std::env::var("LITECR_ENV").unwrap_or_else(|_| "production".to_owned());
    let environment = match mode.as_str() {
        "production" => AccessEnvironment::Production,
        "development" => AccessEnvironment::Development,
        _ => return Err("LITECR_ENV must be production or development".to_owned()),
    };
    if environment == AccessEnvironment::Development && !bind.ip().is_loopback() {
        return Err("development mode requires a loopback LITECR_BIND".to_owned());
    }

    let issuer_url = std::env::var("LITECR_OIDC_ISSUER_URL")
        .map_err(|_| "LITECR_OIDC_ISSUER_URL is required for serve".to_owned())?;
    let client_id = std::env::var("LITECR_OIDC_CLIENT_ID")
        .map_err(|_| "LITECR_OIDC_CLIENT_ID is required for serve".to_owned())?;
    let client_secret = std::env::var("LITECR_OIDC_CLIENT_SECRET")
        .map_err(|_| "LITECR_OIDC_CLIENT_SECRET is required for serve".to_owned())?;
    let redirect_uri = std::env::var("LITECR_OIDC_REDIRECT_URI")
        .map_err(|_| "LITECR_OIDC_REDIRECT_URI is required for serve".to_owned())?;
    let expected_origin = std::env::var("LITECR_WEB_ORIGIN")
        .map_err(|_| "LITECR_WEB_ORIGIN is required for serve".to_owned())?;
    let expected_callback = format!("{expected_origin}/api/v1/access/oidc/callback");
    if redirect_uri != expected_callback {
        return Err("LITECR_OIDC_REDIRECT_URI must match LITECR_WEB_ORIGIN and the registered callback path".to_owned());
    }

    let secure_cookies = match std::env::var("LITECR_COOKIE_SECURE") {
        Ok(value) if value == "true" => true,
        Ok(value) if value == "false" => false,
        Ok(_) => return Err("LITECR_COOKIE_SECURE must be true or false".to_owned()),
        Err(_) => environment == AccessEnvironment::Production,
    };
    if environment == AccessEnvironment::Production && !secure_cookies {
        return Err("production mode requires LITECR_COOKIE_SECURE=true".to_owned());
    }
    let session_lifetime_seconds =
        parse_duration_env("LITECR_SESSION_LIFETIME_SECONDS", 28_800, 1, 86_400)?;
    let session_idle_seconds = parse_duration_env("LITECR_SESSION_IDLE_SECONDS", 900, 60, 1800)?;
    let flow_lifetime_seconds =
        parse_duration_env("LITECR_OIDC_FLOW_LIFETIME_SECONDS", 300, 1, 900)?;
    let service_config = AccessServiceConfig {
        login_flow_lifetime_seconds: flow_lifetime_seconds,
        session_lifetime_seconds,
    };
    let api_config = AccessApiConfig {
        expected_origin,
        secure_cookies,
        environment,
        session_lifetime_seconds,
        flow_lifetime_seconds,
    };
    if !api_config.validate() {
        return Err("invalid access origin or session security configuration".to_owned());
    }

    let provider = OidcProviderClient::discover(OidcProviderConfig {
        issuer_url,
        client_id,
        client_secret: Some(client_secret),
        redirect_uri,
        allow_http_loopback: environment == AccessEnvironment::Development
            && bind.ip().is_loopback(),
    })
    .await
    .map_err(|_| {
        "OIDC provider discovery failed; verify issuer and network configuration".to_owned()
    })?;

    let directory = PostgresAccessDirectory::new(pool.clone());
    let session_repository = PostgresAccessSessionRepository::new(pool.clone(), directory.clone())
        .with_idle_timeout_seconds(session_idle_seconds)
        .ok_or_else(|| "invalid session inactivity timeout".to_owned())?;
    let sessions = Arc::new(session_repository.clone());
    let flow_store = Arc::new(PostgresOidcFlowStore::new(pool.clone()));
    let authenticator = Arc::new(PostgresAccessAuthenticator::new(session_repository));
    let service = AccessService::new(
        Arc::new(provider),
        flow_store,
        sessions,
        Arc::new(directory),
        authenticator,
        Arc::new(PostgresAccessSecurityAudit::new(pool)),
        service_config,
    )
    .ok_or_else(|| "invalid access session configuration".to_owned())?;
    let service = Arc::new(service);

    Ok((
        access_router(service.clone(), api_config.clone()),
        service,
        api_config,
    ))
}

fn matching_parallelism(value: &str) -> Result<usize, String> {
    value
        .parse::<usize>()
        .ok()
        .filter(|n| (1..=128).contains(n))
        .ok_or_else(|| "LITECR_MATCHING_PARALLELISM must be between 1 and 128".to_owned())
}

fn configured_matching_store(pool: PgPool) -> Result<PostgresMatchingStore, String> {
    match std::env::var("LITECR_MATCHING_PARALLELISM") {
        Ok(value) => {
            PostgresMatchingStore::with_evaluation_parallelism(pool, matching_parallelism(&value)?)
                .map_err(str::to_owned)
        }
        Err(std::env::VarError::NotPresent) => Ok(PostgresMatchingStore::new(pool)),
        Err(_) => Err("LITECR_MATCHING_PARALLELISM must be valid UTF-8".to_owned()),
    }
}

fn compose_features(
    pool: PgPool,
    access_service: Arc<AccessService>,
    access_config: AccessApiConfig,
) -> Result<axum::Router, String> {
    let registry = RegistryService::new(Box::new(PostgresRegistryStore::new(pool.clone())), 100)
        .ok_or_else(|| "invalid registry page-size configuration".to_owned())?;
    let registry_service = Arc::new(registry);
    let registry = protected_routes(
        litecr_api::registry::router(registry_service.clone()),
        access_service.clone(),
        access_config.clone(),
    );
    let fhir = protected_routes(
        litecr_api::integrations::fhir::router(registry_service, fhir_client_bindings()?),
        access_service.clone(),
        access_config.clone(),
    );

    let matching = MatchingService::new(Box::new(configured_matching_store(pool.clone())?), 200)
        .ok_or_else(|| "invalid matching candidate limit configuration".to_owned())?;
    let matching = protected_routes(
        litecr_api::matching::router(Arc::new(matching)),
        access_service.clone(),
        access_config.clone(),
    );

    let review = ReviewService::with_access_directory(
        Box::new(PostgresReviewStore::new(pool.clone())),
        Arc::new(PostgresAccessDirectory::new(pool.clone())),
    );
    let review = protected_routes(
        litecr_api::review::router(Arc::new(review)),
        access_service.clone(),
        access_config.clone(),
    );

    let migration = MigrationService::new(Box::new(PostgresMigrationExecutor::new(pool.clone())));
    let migration = protected_routes(
        litecr_api::migration::router_with_package_v2(
            Arc::new(migration),
            migration_package_policy_from_env()?,
        ),
        access_service.clone(),
        access_config.clone(),
    );

    let catalog = Arc::new(CatalogService::new(Arc::new(
        PostgresCatalogRepository::new(pool.clone()),
    )));
    let localization_public = litecr_api::localization::public_router(catalog.clone());
    let localization_protected = protected_routes(
        litecr_api::localization::protected_router(catalog),
        access_service.clone(),
        access_config.clone(),
    );

    let country_profiles = Arc::new(CountryProfileService::new(Arc::new(
        PostgresCountryProfileRepository::new(pool.clone()),
    )));
    let country_public = litecr_api::country_profiles::public_router(country_profiles.clone());
    let country_protected = protected_routes(
        litecr_api::country_profiles::protected_router(country_profiles),
        access_service.clone(),
        access_config.clone(),
    );

    let appearance = Arc::new(AppearanceService::new(Arc::new(
        PostgresAppearanceRepository::new(pool),
    )));
    let appearance_public = litecr_api::appearance::public_router(appearance.clone());
    let appearance_protected = protected_routes(
        litecr_api::appearance::protected_router(appearance),
        access_service,
        access_config,
    );

    Ok(registry
        .merge(fhir)
        .merge(matching)
        .merge(review)
        .merge(migration)
        .merge(localization_public)
        .merge(localization_protected)
        .merge(country_public)
        .merge(country_protected)
        .merge(appearance_public)
        .merge(appearance_protected))
}

fn migration_package_policy_from_env() -> Result<Option<MigrationPackagePolicy>, String> {
    let names = [
        "LITECR_MIGRATION_V2_STAGE_QUOTA_BYTES",
        "LITECR_MIGRATION_V2_MAX_ACTIVE_UPLOADS",
        "LITECR_MIGRATION_V2_STAGE_RETENTION_SECONDS",
        "LITECR_MIGRATION_V2_STORAGE_PROTECTION_REFERENCE",
    ];
    let values = names.map(std::env::var);
    if values
        .iter()
        .all(|value| matches!(value, Err(std::env::VarError::NotPresent)))
    {
        return Ok(None);
    }
    if values.iter().any(Result::is_err) {
        return Err(
            "server-verified migration v2 requires all staging policy settings or none".into(),
        );
    }
    let quota = values[0]
        .as_ref()
        .unwrap()
        .parse::<u64>()
        .map_err(|_| "LITECR_MIGRATION_V2_STAGE_QUOTA_BYTES must be a positive integer")?;
    let max_active = values[1]
        .as_ref()
        .unwrap()
        .parse::<u32>()
        .map_err(|_| "LITECR_MIGRATION_V2_MAX_ACTIVE_UPLOADS must be a positive integer")?;
    let retention =
        values[2].as_ref().unwrap().parse::<u64>().map_err(
            |_| "LITECR_MIGRATION_V2_STAGE_RETENTION_SECONDS must be a positive integer",
        )?;
    let protection = values[3].as_ref().unwrap().trim().to_owned();
    if quota == 0
        || quota > i64::MAX as u64
        || max_active == 0
        || retention == 0
        || retention > i64::MAX as u64
        || protection.is_empty()
        || protection.len() > 512
        || protection.chars().any(char::is_control)
    {
        return Err("server-verified migration v2 staging policy values are invalid".into());
    }
    Ok(Some(MigrationPackagePolicy {
        global_quota_bytes: quota,
        max_active_uploads: max_active,
        retention_seconds: retention,
        storage_protection_reference: protection,
    }))
}

fn fhir_client_bindings()
-> Result<HashMap<String, litecr_api::integrations::fhir::FhirClientBinding>, String> {
    let raw = match std::env::var("LITECR_FHIR_CLIENT_BINDINGS") {
        Ok(value) => value,
        Err(std::env::VarError::NotPresent) => return Ok(HashMap::new()),
        Err(_) => return Err("LITECR_FHIR_CLIENT_BINDINGS is not valid Unicode".to_owned()),
    };
    parse_fhir_client_bindings(&raw)
}

fn parse_fhir_client_bindings(
    raw: &str,
) -> Result<HashMap<String, litecr_api::integrations::fhir::FhirClientBinding>, String> {
    let parsed: serde_json::Value = serde_json::from_str(raw)
        .map_err(|_| "LITECR_FHIR_CLIENT_BINDINGS must be a JSON object".to_owned())?;
    let object = parsed
        .as_object()
        .filter(|items| items.len() <= 1000)
        .ok_or_else(|| {
            "LITECR_FHIR_CLIENT_BINDINGS must contain at most 1000 client entries".to_owned()
        })?;
    let mut bindings = HashMap::with_capacity(object.len());
    for (identity, value) in object {
        let (issuer, subject) = identity
            .split_once('#')
            .filter(|(_, subject)| !subject.is_empty())
            .ok_or_else(|| "FHIR binding keys must use issuer#subject".to_owned())?;
        if !(issuer.is_empty()
            || issuer.starts_with("https://")
            || issuer.starts_with("http://127.0.0.1:")
            || issuer.starts_with("http://localhost:"))
            || identity.len() > 4096
            || subject.bytes().any(|byte| byte.is_ascii_control())
        {
            return Err("FHIR binding identity is invalid".to_owned());
        }
        let field = |name: &str, maximum: usize| -> Result<String, String> {
            let text = value
                .get(name)
                .and_then(serde_json::Value::as_str)
                .filter(|text| !text.trim().is_empty() && text.len() <= maximum)
                .ok_or_else(|| format!("FHIR binding field {name} is missing or invalid"))?;
            if text.bytes().any(|byte| byte.is_ascii_control()) {
                return Err(format!("FHIR binding field {name} is invalid"));
            }
            Ok(text.to_owned())
        };
        let create_enabled = value
            .get("create_enabled")
            .and_then(serde_json::Value::as_bool)
            .ok_or_else(|| "FHIR binding create_enabled must be boolean".to_owned())?;
        bindings.insert(
            identity.clone(),
            litecr_api::integrations::fhir::FhirClientBinding {
                registry_id: field("registry_id", 128)?,
                organization_id: field("organization_id", 128)?,
                source_system_id: field("source_system_id", 128)?,
                create_enabled,
            },
        );
    }
    Ok(bindings)
}

fn parse_duration_env(name: &str, default: i64, minimum: i64, maximum: i64) -> Result<i64, String> {
    let value = std::env::var(name).unwrap_or_else(|_| default.to_string());
    value
        .parse::<i64>()
        .ok()
        .filter(|value| (minimum..=maximum).contains(value))
        .ok_or_else(|| format!("{name} must be between {minimum} and {maximum}"))
}

async fn run() -> Result<(), String> {
    let cli = Cli::parse();
    let command = cli.command;
    if let Command::Version { json } = &command {
        if *json {
            println!(
                "{}",
                serde_json::json!({
                    "application_version": env!("CARGO_PKG_VERSION"),
                    "schema_version": SCHEMA_VERSION,
                })
            );
        } else {
            println!(
                "litecr {} schema {}",
                env!("CARGO_PKG_VERSION"),
                SCHEMA_VERSION
            );
        }
        return Ok(());
    }

    match command {
        Command::Version { .. } => unreachable!(),
        Command::Migration { command } => match *command {
            migration_cli::MigrationCommand::CleanupV2 { limit } => {
                let config = DatabaseConfig::from_env().map_err(str::to_owned)?;
                let pool = open_database(&config).await?;
                require_compatible_schema(&pool).await?;
                let executor = PostgresMigrationExecutor::new(pool);
                let expired = executor
                    .cleanup_expired_packages_v2(limit)
                    .await
                    .map_err(|_| "expired migration staging cleanup failed".to_owned())?;
                println!("expired v2 migration uploads cleaned: {expired}");
            }
            command => migration_cli::run(command).await?,
        },
        Command::Worker => worker_cli::run().await?,
        Command::ServiceApi => {
            #[cfg(windows)]
            windows_service::dispatch(windows_service::ServiceMode::Api)?;
            #[cfg(not(windows))]
            return Err("Windows SCM service modes are supported only on Windows".to_owned());
        }
        Command::ServiceWorker => {
            #[cfg(windows)]
            windows_service::dispatch(windows_service::ServiceMode::Worker)?;
            #[cfg(not(windows))]
            return Err("Windows SCM service modes are supported only on Windows".to_owned());
        }
        Command::Matching { command } => match command {
            MatchingCommand::Reindex {
                batch_size,
                registry,
                organization,
            } => {
                let config = DatabaseConfig::from_env().map_err(str::to_owned)?;
                let pool = open_database(&config).await?;
                let matching = PostgresMatchingStore::new(pool.clone());
                let mut total = 0usize;
                loop {
                    let indexed = matching
                        .reindex_next_batch(registry.as_deref(), organization.as_deref(), batch_size)
                        .await
                        .map_err(|_| "matching reindex batch failed; retry the command after checking database access".to_owned())?;
                    if indexed == 0 {
                        break;
                    }
                    total += indexed;
                    info!(
                        indexed_in_batch = indexed,
                        total_indexed = total,
                        "matching index backfill progress"
                    );
                }
                println!(
                    "matching index ready for {total} previously unindexed accepted source versions"
                );
            }
        },
        Command::Migrate => {
            let migration_config = DatabaseConfig::migration_from_env().map_err(str::to_owned)?;
            let pool = open_database(&migration_config).await?;
            migrate(&pool)
                .await
                .map_err(|_| "embedded database migration failed".to_owned())?;
            info!(
                schema_version = SCHEMA_VERSION,
                "database migrations applied"
            );
        }
        Command::Doctor => {
            let config = DatabaseConfig::from_env().map_err(str::to_owned)?;
            let pool = open_database(&config).await?;
            let version = require_compatible_schema(&pool).await?;
            let replication_status = synchronous_replication_status(&pool)
                .await
                .map_err(|_| "could not check PostgreSQL replication policy".to_owned())?;
            println!(
                "database: reachable; schema: compatible ({})",
                version.value()
            );
            let pending_reindex = PostgresMatchingStore::new(pool.clone())
                .pending_reindex_count()
                .await
                .map_err(|_| "could not check matching index status".to_owned())?;
            println!("{}", doctor_matching_index_summary(pending_reindex));
            println!("{}", doctor_replication_summary(replication_status));
        }
        Command::Serve => {
            run_api_with_shutdown(shutdown_signal(), || {}).await?;
        }
    }
    Ok(())
}

pub(crate) async fn run_api_with_shutdown<S, R>(shutdown: S, on_ready: R) -> Result<(), String>
where
    S: Future<Output = ()> + Send + 'static,
    R: FnOnce() + Send + 'static,
{
    let config = DatabaseConfig::from_env().map_err(str::to_owned)?;
    let bind = configured_bind().map_err(str::to_owned)?;
    let pool = open_database(&config).await?;
    let version = require_compatible_schema(&pool).await?;
    info!(
        schema_version = version.value(),
        "database schema is compatible"
    );
    let readiness = Arc::new(PostgresReadinessProbe::new(pool.clone()));
    let (access_router, access_service, access_config) = compose_access(pool.clone(), bind).await?;
    let features = compose_features(pool, access_service, access_config)?;
    let app = router(readiness)
        .merge(access_router)
        .merge(features)
        .fallback(embedded_asset_fallback);
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .map_err(|_| "could not bind configured address".to_owned())?;
    info!(address = %bind, "liteCR HTTP service listening");
    on_ready();
    serve_with_shutdown(listener, app, shutdown).await
}

async fn serve_with_shutdown<F>(
    listener: tokio::net::TcpListener,
    app: axum::Router,
    shutdown: F,
) -> Result<(), String>
where
    F: Future<Output = ()> + Send + 'static,
{
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown)
        .await
        .map_err(|_| "HTTP service stopped unexpectedly".to_owned())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut signal) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            signal.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}

async fn embedded_asset_fallback(method: Method, uri: Uri) -> Response {
    if method != Method::GET && method != Method::HEAD {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }
    let requested_path = uri.path();
    let app_route = !requested_path.starts_with("/api/")
        && requested_path != "/api"
        && !requested_path.starts_with("/health/")
        && requested_path != "/health"
        && !requested_path.starts_with("/fhir/")
        && requested_path != "/fhir"
        && !requested_path.starts_with("/assets/")
        && !requested_path
            .rsplit('/')
            .next()
            .unwrap_or_default()
            .contains('.');
    let asset_path = if requested_path == "/" || app_route {
        "/index.html"
    } else {
        requested_path
    };
    let Some((_, bytes)) = embedded_web::EMBEDDED_WEB_ASSETS
        .iter()
        .find(|(path, _)| *path == asset_path)
    else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let content_type = match asset_path.rsplit('.').next().unwrap_or_default() {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "json" => "application/json",
        _ => "application/octet-stream",
    };
    let cache_control = if asset_path == "/index.html" {
        "no-cache"
    } else {
        "public, max-age=31536000, immutable"
    };
    let body = if method == Method::HEAD {
        Body::empty()
    } else {
        Body::from(*bytes)
    };
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CONTENT_LENGTH, bytes.len().to_string())
        .header(header::CACHE_CONTROL, cache_control)
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        .body(body)
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

#[tokio::main]
async fn main() {
    init_tracing();
    if let Err(error_message) = run().await {
        error!(error = %error_message, "liteCR command failed");
        std::process::exit(2);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matching_parallelism_rejects_unbounded_or_invalid_values() {
        for value in ["", "0", "129", "unlimited", "-1", " 4", "4 "] {
            assert!(matching_parallelism(value).is_err());
        }
        for value in ["1", "2", "4", "8", "128"] {
            assert_eq!(matching_parallelism(value).unwrap().to_string(), value);
        }
    }

    #[tokio::test]
    async fn api_gracefully_stops_when_injected_shutdown_signal_fires() {
        use axum::{Router, routing::get};
        use tokio::sync::oneshot;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        let server = tokio::spawn(serve_with_shutdown(
            listener,
            Router::new().route("/", get(|| async { "synthetic-response" })),
            async move {
                let _ = shutdown_rx.await;
            },
        ));

        let response = reqwest::get(format!("http://{address}/")).await.unwrap();
        assert_eq!(response.text().await.unwrap(), "synthetic-response");
        shutdown_tx.send(()).unwrap();

        tokio::time::timeout(std::time::Duration::from_secs(2), server)
            .await
            .expect("server should stop after the injected signal")
            .unwrap()
            .unwrap();
    }

    #[test]
    fn worker_is_an_explicit_standalone_subcommand() {
        let parsed = Cli::try_parse_from(["litecr", "worker"]).expect("worker subcommand parses");
        assert!(matches!(parsed.command, Command::Worker));
        assert!(Cli::try_parse_from(["litecr", "serve", "worker"]).is_err());
    }

    #[test]
    fn windows_scm_modes_are_explicit_and_distinct_from_migrations() {
        let api = Cli::try_parse_from(["litecr", "service-api"]).expect("API SCM mode parses");
        let worker =
            Cli::try_parse_from(["litecr", "service-worker"]).expect("worker SCM mode parses");
        assert!(matches!(&api.command, Command::ServiceApi));
        assert!(matches!(&worker.command, Command::ServiceWorker));
        assert!(!matches!(&api.command, Command::Migrate));
        assert!(!matches!(&worker.command, Command::Migrate));
    }

    #[test]
    fn migration_v2_cleanup_is_a_bounded_native_operator_command() {
        let parsed = Cli::try_parse_from(["litecr", "migration", "cleanup-v2", "--limit", "250"])
            .expect("bounded cleanup command parses");
        assert!(matches!(
            parsed.command,
            Command::Migration { command }
                if matches!(*command, migration_cli::MigrationCommand::CleanupV2 { limit: 250 })
        ));
        assert!(
            Cli::try_parse_from(["litecr", "migration", "cleanup-v2", "--limit", "0",]).is_err()
        );
        assert!(
            Cli::try_parse_from(["litecr", "migration", "cleanup-v2", "--limit", "1001",]).is_err()
        );
    }

    #[test]
    fn fhir_bindings_require_scoped_composite_identity_and_typed_fields() {
        let valid = r#"{"https://issuer.example#client-a":{"registry_id":"reg-a","organization_id":"org-a","source_system_id":"source-a","create_enabled":false}}"#;
        let bindings = parse_fhir_client_bindings(valid).expect("valid fixture binding");
        assert_eq!(bindings.len(), 1);
        assert!(!bindings["https://issuer.example#client-a"].create_enabled);

        for invalid in [
            r#"{"client-a":{"registry_id":"r","organization_id":"o","source_system_id":"s","create_enabled":false}}"#,
            r#"{"https://issuer.example#client-a":{"registry_id":"r","organization_id":"o","source_system_id":"s","create_enabled":"true"}}"#,
            r#"["https://issuer.example#client-a"]"#,
        ] {
            assert!(parse_fhir_client_bindings(invalid).is_err());
        }
    }

    #[test]
    fn configuration_rejects_invalid_values_without_echoing_secrets() {
        let invalid =
            DatabaseConfig::parse("postgres://user:secret@db.invalid/litecr".to_owned(), "0");
        let Err(invalid) = invalid else {
            panic!("zero connections should be invalid");
        };
        assert!(invalid.contains("between 1 and 100"));
        assert!(!invalid.contains("secret"));

        let Err(invalid) =
            DatabaseConfig::parse("mysql://user:secret@db.invalid/litecr".to_owned(), "4")
        else {
            panic!("non-PostgreSQL URL should be invalid");
        };
        assert!(invalid.contains("postgres or postgresql"));
    }

    #[test]
    fn production_bind_is_loopback_only() {
        let public: SocketAddr = "0.0.0.0:8080".parse().unwrap();
        let private: SocketAddr = "127.0.0.1:8080".parse().unwrap();
        assert!(!private.is_ipv4() || private.ip().is_loopback());
        assert!(!public.ip().is_loopback());
    }

    #[test]
    fn api_schema_gate_accepts_only_the_exact_release_schema() {
        assert_eq!(
            validate_schema_version(Some(SchemaVersion::new(SCHEMA_VERSION)))
                .expect("current schema is compatible")
                .value(),
            SCHEMA_VERSION
        );
        assert!(
            validate_schema_version(Some(SchemaVersion::new(SCHEMA_VERSION - 1)))
                .unwrap_err()
                .contains("older than application schema")
        );
        assert!(
            validate_schema_version(None)
                .unwrap_err()
                .contains("schema is not installed")
        );
    }

    #[test]
    fn api_schema_gate_rejects_newer_schema_without_suggesting_migration() {
        let error =
            validate_schema_version(Some(SchemaVersion::new(SCHEMA_VERSION + 1))).unwrap_err();
        assert!(error.contains("newer than application schema"));
        assert!(!error.contains("run litecr migrate"));
    }

    #[test]
    fn doctor_replication_summary_does_not_claim_live_protection() {
        assert_eq!(
            doctor_replication_summary(litecr_infrastructure::SynchronousReplicationStatus {
                policy_configured: false,
                streaming_standbys: 0,
                synchronous_standbys: 0,
                quorum_candidates: 0,
                asynchronous_standbys: 0,
                potential_standbys: 0,
            }),
            "replication: no synchronous standby policy configured; commits rely on local WAL flush only; observed streaming standbys=0 (sync=0, quorum_candidates=0, async=0, potential=0)"
        );
        let configured =
            doctor_replication_summary(litecr_infrastructure::SynchronousReplicationStatus {
                policy_configured: true,
                streaming_standbys: 3,
                synchronous_standbys: 1,
                quorum_candidates: 1,
                asynchronous_standbys: 1,
                potential_standbys: 0,
            });
        assert!(configured.contains("observed streaming standbys=3 (sync=1"));
        assert!(configured.contains("fencing, and RPO/RTO are not verified"));
        assert!(!configured.contains("standby_name"));
    }

    #[tokio::test]
    async fn embedded_web_handler_serves_app_routes_without_masking_api_404s() {
        use axum::body::to_bytes;

        let index =
            embedded_asset_fallback(Method::GET, Uri::from_static("/registry/search")).await;
        assert_eq!(index.status(), StatusCode::OK);
        assert_eq!(
            index.headers()[header::CONTENT_TYPE],
            "text/html; charset=utf-8"
        );
        let body = to_bytes(index.into_body(), 1024 * 1024).await.unwrap();
        assert!(std::str::from_utf8(&body).unwrap().contains("liteCR"));

        let api_missing =
            embedded_asset_fallback(Method::GET, Uri::from_static("/api/v1/not-a-route")).await;
        assert_eq!(api_missing.status(), StatusCode::NOT_FOUND);

        let asset = embedded_web::EMBEDDED_WEB_ASSETS
            .iter()
            .find(|(path, _)| path.starts_with("/assets/") && path.ends_with(".js"))
            .expect("built JavaScript asset exists");
        let response = embedded_asset_fallback(Method::HEAD, Uri::from_static(asset.0)).await;
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[test]
    fn doctor_matching_index_summary_gives_action_when_backfill_is_pending() {
        assert_eq!(doctor_matching_index_summary(0), "matching index: current");
        let pending = doctor_matching_index_summary(3);
        assert!(pending.contains("3 accepted source version(s) need indexing"));
        assert!(pending.contains("litecr matching reindex"));
    }
}
