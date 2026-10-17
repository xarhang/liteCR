//! End-to-end synthetic reconciliation for the protected migration API.
//!
//! This deliberately queries PostgreSQL directly rather than trusting API
//! receipts or the importer's result structures for the before/after counts.

use axum::Router;
use litecr_api::access::{AccessApiConfig, AccessEnvironment};
use litecr_application::access::{
    AccessService, AccessServiceConfig, OidcAuthorization, OidcFlow, OidcIdentityClaims,
    OidcProvider, OidcProviderError,
};
use litecr_infrastructure::{
    access::{
        PostgresAccessAuthenticator, PostgresAccessDirectory, PostgresAccessSessionRepository,
        PostgresOidcFlowStore,
    },
    connect, migrate,
    migration::PostgresMigrationExecutor,
    registry::PostgresRegistryStore,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{FromRow, Row};
use std::{
    collections::HashMap, fs, future::Future, path::Path, path::PathBuf, pin::Pin, process::Command,
};
use std::{net::SocketAddr, sync::Arc, time::SystemTime};
use tokio::{net::TcpListener, sync::oneshot, task::JoinHandle};

const SOURCE_SYSTEM: &str = "synthetic-migration-api";
const TEST_DATABASE_ENV: &str = "LITECR_TEST_DATABASE_URL";

fn suffix() -> String {
    format!(
        "{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("clock is after epoch")
            .as_nanos()
    )
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(DIGITS[(byte >> 4) as usize] as char);
        output.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    output
}

#[derive(Clone)]
struct SyntheticRecord {
    source_key: String,
    public_id: String,
    cruid: String,
    alias: String,
    identifier: String,
    raw_ndjson: String,
    payload_sha256: String,
}

fn synthetic_record(prefix: &str, key: &str) -> Result<SyntheticRecord, String> {
    let source_key = format!("{prefix}-source-{key}");
    let public_id = format!("{prefix}-public-{key}");
    let cruid = format!("{prefix}-cruid-{key}");
    let alias = format!("{prefix}-alias-{key}");
    let identifier = format!("{prefix}-mrn-{key}");
    let row = json!({
        "source_system_id": SOURCE_SYSTEM,
        "source_record_key": source_key,
        "source_version": "v1",
        "public_id": public_id,
        "cruid": cruid,
        "aliases": [alias],
        "assertions": [
            {
                "field_path": "name.original",
                "original_value": format!("ຊື່ທົດສອບ {key}"),
                "language_tag": "lo",
                "script": "Lao"
            },
            {
                "field_path": "birthDate",
                "original_value": "1984-06",
                "partial_date_precision": "month"
            }
        ],
        "identifiers": [{
            "authority": "urn:litecr:test:clinic",
            "type": "MRN",
            "value": identifier,
            "authoritative": true
        }],
        "relationships": []
    });
    let canonical = serde_json::to_vec(&row)
        .map_err(|_| "synthetic row serializes as canonical JSON".to_owned())?;
    let raw_ndjson = format!(
        "{}\n",
        String::from_utf8(canonical.clone())
            .map_err(|_| "synthetic row serializes as UTF-8".to_owned())?
    );
    Ok(SyntheticRecord {
        source_key: format!("{prefix}-source-{key}"),
        public_id: format!("{prefix}-public-{key}"),
        cruid: format!("{prefix}-cruid-{key}"),
        alias: format!("{prefix}-alias-{key}"),
        identifier: format!("{prefix}-mrn-{key}"),
        raw_ndjson,
        payload_sha256: hex(&sha256(&canonical)),
    })
}

#[derive(Clone)]
struct SyntheticRun {
    manifest_hash: [u8; 32],
    manifest_hex: String,
    body: Value,
    records: Vec<SyntheticRecord>,
}

fn synthetic_run(
    name: &str,
    registry_id: &str,
    organization_id: &str,
    records: Vec<SyntheticRecord>,
) -> SyntheticRun {
    let stream: Vec<u8> = records
        .iter()
        .flat_map(|record| record.raw_ndjson.as_bytes().iter().copied())
        .collect();
    let manifest_hash = sha256(format!("litecr-synthetic-manifest-{name}").as_bytes());
    let manifest_hex = hex(&manifest_hash);
    let body = json!({
        "manifest_sha256": manifest_hex,
        "stream_sha256": hex(&sha256(&stream)),
        "snapshot_id": format!("synthetic-snapshot-{name}"),
        "source_system_id": SOURCE_SYSTEM,
        "registry_id": registry_id,
        "organization_id": organization_id,
        "expected_record_count": records.len()
    });
    SyntheticRun {
        manifest_hash,
        manifest_hex,
        body,
        records,
    }
}

struct CliFixture {
    root: PathBuf,
    manifest: PathBuf,
    stream: PathBuf,
    stage: PathBuf,
    credential: PathBuf,
    manifest_sha256: String,
}

impl Drop for CliFixture {
    fn drop(&mut self) {
        let marker = self.root.join(".litecr-cli-test-owner");
        if fs::read_to_string(&marker)
            .is_ok_and(|owner| owner == "litecr-migration-api-reconciliation-v1")
            && self
                .root
                .parent()
                .is_some_and(|parent| parent == std::env::temp_dir())
        {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
}

#[cfg(unix)]
fn restrict_cli_fixture_permissions(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        return Err(std::io::Error::other("fixture contains a symlink"));
    }
    fs::set_permissions(
        path,
        fs::Permissions::from_mode(if metadata.is_dir() { 0o700 } else { 0o600 }),
    )?;
    if metadata.is_dir() {
        for entry in fs::read_dir(path)? {
            restrict_cli_fixture_permissions(&entry?.path())?;
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn restrict_cli_fixture_permissions(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

fn create_cli_fixture(
    key: &str,
    registry_id: &str,
    organization_id: &str,
    credential: &str,
) -> Result<CliFixture, String> {
    let root = std::env::temp_dir().join(format!("litecr-migration-cli-{key}"));
    fs::create_dir(&root).map_err(|_| "isolated CLI test directory is created".to_owned())?;
    fs::write(
        root.join(".litecr-cli-test-owner"),
        "litecr-migration-api-reconciliation-v1",
    )
    .map_err(|_| "CLI fixture ownership marker is written".to_owned())?;
    let manifest = root.join("manifest.json");
    let stream = root.join("records.ndjson");
    let stage = root.join("stage");
    let credential_file = root.join("operator.credential");
    let record = synthetic_record("cli", key)?;
    let stream_bytes = record.raw_ndjson.as_bytes();
    let stream_sha256 = hex(&sha256(stream_bytes));
    let manifest_bytes = serde_json::to_vec(&json!({
        "format":"litecr-migration/v1",
        "snapshot_id":format!("cli-snapshot-{key}"),
        "source_system_id":SOURCE_SYSTEM,
        "target_registry_id":registry_id,
        "target_organization_id":organization_id,
        "exported_at":"2026-10-12T00:00:00Z",
        "record_count":1,
        "stream_bytes":stream_bytes.len(),
        "stream_sha256":stream_sha256,
    }))
    .map_err(|_| "synthetic CLI manifest serializes".to_owned())?;
    let manifest_sha256 = hex(&sha256(&manifest_bytes));
    fs::write(&manifest, manifest_bytes)
        .map_err(|_| "synthetic CLI manifest is written".to_owned())?;
    fs::write(&stream, stream_bytes).map_err(|_| "synthetic CLI stream is written".to_owned())?;
    fs::write(&credential_file, credential)
        .map_err(|_| "synthetic CLI credential file is written".to_owned())?;

    fs::create_dir(&stage).map_err(|_| "synthetic CLI staging directory is created".to_owned())?;
    fs::create_dir(stage.join(".validation"))
        .map_err(|_| "synthetic CLI validation directory is created".to_owned())?;
    fs::write(stage.join(".validation/index.sqlite3"), [])
        .map_err(|_| "synthetic CLI validation index placeholder is written".to_owned())?;
    let batches = stage.join("batches");
    fs::create_dir(&batches).map_err(|_| "synthetic CLI batch directory is created".to_owned())?;
    fs::write(batches.join("00000001.ndjson"), stream_bytes)
        .map_err(|_| "synthetic CLI staged batch is written".to_owned())?;
    fs::write(
        batches.join("00000001.json"),
        serde_json::to_vec(&json!({
            "sequence":1,
            "manifest_sha256":manifest_sha256,
            "stream_sha256":stream_sha256,
            "batch_sha256":hex(&sha256(stream_bytes)),
            "record_count":1,
            "first_input_line":1,
            "last_input_line":1,
            "status":"staged"
        }))
        .map_err(|_| "synthetic CLI sidecar serializes".to_owned())?,
    )
    .map_err(|_| "synthetic CLI batch sidecar is written".to_owned())?;
    fs::write(
        stage.join("checkpoint.json"),
        serde_json::to_vec(&json!({
            "format":"litecr-migration/v1",
            "state":"staged",
            "manifest_sha256":manifest_sha256,
            "stream_sha256":stream_sha256,
            "stream_bytes":stream_bytes.len(),
            "record_count":1,
            "valid_records":1,
            "exact_replay_duplicates":0,
            "quarantined_records":0,
            "batch_size":1,
            "batch_bytes":8*1024*1024,
            "committed_batch_sequence":0,
            "staged_batch_sequence":1,
            "staged_records":1,
            "next_line":2
        }))
        .map_err(|_| "synthetic CLI checkpoint serializes".to_owned())?,
    )
    .map_err(|_| "synthetic CLI checkpoint is written".to_owned())?;
    fs::write(
        stage.join("quarantine.json"),
        serde_json::to_vec(&json!({
            "format":"litecr-migration/v1",
            "manifest_sha256":manifest_sha256,
            "exceptions":[]
        }))
        .map_err(|_| "synthetic CLI quarantine report serializes".to_owned())?,
    )
    .map_err(|_| "synthetic CLI quarantine report is written".to_owned())?;
    restrict_cli_fixture_permissions(&root).map_err(|_| {
        "synthetic CLI fixture permissions exclude group and other users".to_owned()
    })?;

    Ok(CliFixture {
        root,
        manifest,
        stream,
        stage,
        credential: credential_file,
        manifest_sha256,
    })
}

fn batch_body(run: &SyntheticRun, sequence: u64, record_index: usize) -> Value {
    let record = &run.records[record_index];
    let raw_bytes = record.raw_ndjson.as_bytes();
    json!({
        "run": run.body,
        "sequence": sequence,
        "first_input_line": record_index + 1,
        "last_input_line": record_index + 1,
        "batch_sha256": hex(&sha256(raw_bytes)),
        "records": [{
            "source_system_id": SOURCE_SYSTEM,
            "source_record_key": record.source_key,
            "source_version": "v1",
            "payload_sha256": record.payload_sha256,
            "raw_ndjson": record.raw_ndjson
        }]
    })
}

struct RunningApi {
    base_url: String,
    stop: Option<oneshot::Sender<()>>,
    task: JoinHandle<std::io::Result<()>>,
}

impl RunningApi {
    async fn stop(mut self) -> Result<(), String> {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        self.task
            .await
            .map_err(|_| "protected migration test server task joins".to_owned())?
            .map_err(|_| "protected migration test server exits cleanly".to_owned())
    }
}

async fn serve_migration_api(
    pool: sqlx::PgPool,
    registry_id: &str,
    organization_id: &str,
    subject: &str,
    wrong_registry_id: &str,
    wrong_organization_id: &str,
    wrong_subject: &str,
) -> Result<RunningApi, String> {
    let directory = PostgresAccessDirectory::new(pool.clone());
    let sessions = PostgresAccessSessionRepository::new(pool.clone(), directory.clone());
    let access = AccessService::new(
        Arc::new(UnusedOidcProvider),
        Arc::new(PostgresOidcFlowStore::new(pool.clone())),
        Arc::new(sessions.clone()),
        Arc::new(directory),
        Arc::new(PostgresAccessAuthenticator::new(sessions)),
        Arc::new(litecr_infrastructure::access::PostgresAccessSecurityAudit::new(pool.clone())),
        AccessServiceConfig {
            login_flow_lifetime_seconds: 300,
            session_lifetime_seconds: 600,
        },
    )
    .ok_or_else(|| "synthetic access service configuration is valid".to_owned())?;
    let migration = litecr_application::migration::MigrationService::new(Box::new(
        PostgresMigrationExecutor::new(pool.clone()),
    ));
    let registry = Arc::new(
        litecr_application::registry::RegistryService::new(
            Box::new(PostgresRegistryStore::new(pool)),
            100,
        )
        .ok_or_else(|| "synthetic registry service page size is valid".to_owned())?,
    );
    let fhir_bindings = HashMap::from([
        (
            format!("#{subject}"),
            litecr_api::integrations::fhir::FhirClientBinding {
                registry_id: registry_id.to_owned(),
                organization_id: organization_id.to_owned(),
                source_system_id: SOURCE_SYSTEM.to_owned(),
                create_enabled: false,
            },
        ),
        (
            format!("#{wrong_subject}"),
            litecr_api::integrations::fhir::FhirClientBinding {
                registry_id: wrong_registry_id.to_owned(),
                organization_id: wrong_organization_id.to_owned(),
                source_system_id: SOURCE_SYSTEM.to_owned(),
                create_enabled: false,
            },
        ),
    ]);
    let features = litecr_api::migration::router(Arc::new(migration))
        .merge(litecr_api::registry::router(registry.clone()))
        .merge(litecr_api::integrations::fhir::router(
            registry,
            fhir_bindings,
        ));
    let router = litecr_api::access::protected_routes(
        features,
        Arc::new(access),
        AccessApiConfig {
            expected_origin: "http://127.0.0.1".to_owned(),
            secure_cookies: false,
            environment: AccessEnvironment::Development,
            session_lifetime_seconds: 600,
            flow_lifetime_seconds: 300,
        },
    );
    start_server(router).await
}

async fn get_fhir_identifier(
    client: &reqwest::Client,
    base_url: &str,
    authority: &str,
    value: &str,
    credential: &str,
) -> Result<(reqwest::StatusCode, Value), String> {
    let response = client
        .get(format!("{base_url}/fhir/r4/Patient"))
        .query(&[("identifier", format!("{authority}|{value}"))])
        .bearer_auth(credential)
        .send()
        .await
        .map_err(|_| "synthetic FHIR exact-identifier GET completes".to_owned())?;
    let status = response.status();
    let bytes = response
        .bytes()
        .await
        .map_err(|_| "synthetic FHIR search response can be read".to_owned())?;
    let data = serde_json::from_slice(&bytes)
        .map_err(|_| "FHIR search response is valid JSON".to_owned())?;
    Ok((status, data))
}

async fn start_server(router: Router) -> Result<RunningApi, String> {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|_| "protected migration test server binds to loopback".to_owned())?;
    let address: SocketAddr = listener
        .local_addr()
        .map_err(|_| "protected migration test server address is available".to_owned())?;
    let (stop, stopped) = oneshot::channel();
    let task = tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(async move {
                let _ = stopped.await;
            })
            .await
    });
    Ok(RunningApi {
        base_url: format!("http://{address}"),
        stop: Some(stop),
        task,
    })
}

async fn post_json(
    client: &reqwest::Client,
    base_url: &str,
    path: &str,
    credential: Option<&str>,
    body: &Value,
) -> Result<(reqwest::StatusCode, Value), String> {
    let mut request = client.post(format!("{base_url}{path}")).json(body);
    if let Some(credential) = credential {
        request = request.bearer_auth(credential);
    }
    let response = request
        .send()
        .await
        .map_err(|_| "synthetic protected API request completes".to_owned())?;
    let status = response.status();
    let bytes = response
        .bytes()
        .await
        .map_err(|_| "synthetic protected API response can be read".to_owned())?;
    let data = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes)
            .map_err(|_| "protected API response is valid JSON when non-empty".to_owned())?
    };
    Ok((status, data))
}

async fn run_litecr_cli(args: Vec<String>) -> Result<std::process::Output, String> {
    let binary = env!("CARGO_BIN_EXE_litecr").to_owned();
    tokio::task::spawn_blocking(move || Command::new(binary).args(args).output())
        .await
        .map_err(|_| "synthetic migration CLI subprocess joins".to_owned())?
        .map_err(|_| "synthetic migration CLI subprocess starts".to_owned())
}

async fn provision_integration_client(
    pool: &sqlx::PgPool,
    subject: &str,
    registry_id: &str,
    organization_id: &str,
) -> Result<String, String> {
    sqlx::query(
        "INSERT INTO litecr.access_identity (actor_kind, issuer_url, external_subject) \
         VALUES ('integration_client', '', $1)",
    )
    .bind(subject)
    .execute(pool)
    .await
    .map_err(|_| "unique synthetic integration identity is inserted".to_owned())?;
    for (role, permission) in [
        (Some("integration_client"), None),
        (None, Some("source:write")),
        (None, Some("migration:write")),
        (None, Some("person:search")),
    ] {
        sqlx::query(
            "INSERT INTO litecr.access_grant \
             (actor_kind, issuer_url, external_subject, registry_id, organization_id, role_name, permission_name) \
             VALUES ('integration_client', '', $1, $2, $3, $4, $5)",
        )
        .bind(subject)
        .bind(registry_id)
        .bind(organization_id)
        .bind(role)
        .bind(permission)
        .execute(pool)
        .await
        .map_err(|_| "synthetic client receives separately scoped source-write, migration-write and person-search permissions".to_owned())?;
    }
    let directory = PostgresAccessDirectory::new(pool.clone());
    let sessions = PostgresAccessSessionRepository::new(pool.clone(), directory);
    sessions
        .issue_integration_credential(None, subject, Some(600))
        .await
        .map_err(|_| {
            "synthetic scoped client receives an ephemeral database-backed bearer credential"
                .to_owned()
        })
}

async fn provision_migration_reader(
    pool: &sqlx::PgPool,
    subject: &str,
    registry_id: &str,
    organization_id: &str,
) -> Result<String, String> {
    sqlx::query(
        "INSERT INTO litecr.access_identity (actor_kind, issuer_url, external_subject) \
         VALUES ('integration_client', '', $1)",
    )
    .bind(subject)
    .execute(pool)
    .await
    .map_err(|_| "unique synthetic migration-reader identity is inserted".to_owned())?;
    sqlx::query(
        "INSERT INTO litecr.access_grant \
         (actor_kind, issuer_url, external_subject, registry_id, organization_id, role_name, permission_name) \
         VALUES ('integration_client', '', $1, $2, $3, NULL, 'migration:read')",
    )
    .bind(subject)
    .bind(registry_id)
    .bind(organization_id)
    .execute(pool)
    .await
    .map_err(|_| "synthetic reader receives only the scoped migration:read permission".to_owned())?;
    let directory = PostgresAccessDirectory::new(pool.clone());
    let sessions = PostgresAccessSessionRepository::new(pool.clone(), directory);
    sessions
        .issue_integration_credential(None, subject, Some(600))
        .await
        .map_err(|_| {
            "synthetic scoped reader receives an ephemeral database-backed bearer credential"
                .to_owned()
        })
}

#[derive(Debug, Default, PartialEq, Eq, FromRow)]
struct Snapshot {
    source_records: i64,
    active_sources: i64,
    withdrawn_target_sources: i64,
    source_versions: i64,
    withdrawn_target_versions: i64,
    assertions: i64,
    withdrawn_target_assertions: i64,
    people: i64,
    aliases: i64,
    identifiers: i64,
    target_identifiers: i64,
    control_identifiers: i64,
    imported_audits: i64,
    withdrawal_audits: i64,
    imported_events: i64,
    withdrawal_events: i64,
    reverse_receipts: i64,
}

async fn snapshot(
    pool: &sqlx::PgPool,
    registry_id: &str,
    organization_id: &str,
    target: &SyntheticRun,
    control: &SyntheticRun,
) -> Result<Snapshot, String> {
    let target_keys: Vec<String> = target
        .records
        .iter()
        .map(|record| record.source_key.clone())
        .collect();
    let control_keys: Vec<String> = control
        .records
        .iter()
        .map(|record| record.source_key.clone())
        .collect();
    let all_keys: Vec<String> = target_keys
        .iter()
        .chain(control_keys.iter())
        .cloned()
        .collect();
    let public_ids: Vec<String> = target
        .records
        .iter()
        .chain(control.records.iter())
        .map(|record| record.public_id.clone())
        .collect();
    let aliases: Vec<String> = target
        .records
        .iter()
        .chain(control.records.iter())
        .flat_map(|record| {
            [
                record.public_id.clone(),
                record.cruid.clone(),
                record.alias.clone(),
            ]
        })
        .collect();
    let identifiers: Vec<String> = target
        .records
        .iter()
        .chain(control.records.iter())
        .map(|record| record.identifier.clone())
        .collect();
    let target_identifiers: Vec<String> = target
        .records
        .iter()
        .map(|record| record.identifier.clone())
        .collect();
    let control_identifiers: Vec<String> = control
        .records
        .iter()
        .map(|record| record.identifier.clone())
        .collect();

    sqlx::query_as::<_, Snapshot>(
        "SELECT \
          (SELECT COUNT(*) FROM litecr.source_record s WHERE s.registry_id=$1 AND s.organization_id=$2 AND s.source_system_id=$3 AND s.source_record_key=ANY($4::text[])) AS source_records, \
          (SELECT COUNT(*) FROM litecr.source_record s WHERE s.registry_id=$1 AND s.organization_id=$2 AND s.source_system_id=$3 AND s.source_record_key=ANY($4::text[]) AND s.withdrawn_at IS NULL) AS active_sources, \
          (SELECT COUNT(*) FROM litecr.source_record s WHERE s.registry_id=$1 AND s.organization_id=$2 AND s.source_system_id=$3 AND s.source_record_key=ANY($5::text[]) AND s.withdrawn_at IS NOT NULL) AS withdrawn_target_sources, \
          (SELECT COUNT(*) FROM litecr.source_record_version v JOIN litecr.source_record s USING (source_record_id) WHERE s.registry_id=$1 AND s.organization_id=$2 AND s.source_system_id=$3 AND s.source_record_key=ANY($4::text[])) AS source_versions, \
          (SELECT COUNT(*) FROM litecr.source_record_version v JOIN litecr.source_record s USING (source_record_id) WHERE s.registry_id=$1 AND s.organization_id=$2 AND s.source_system_id=$3 AND s.source_record_key=ANY($5::text[]) AND v.withdrawn_by_manifest=$12) AS withdrawn_target_versions, \
          (SELECT COUNT(*) FROM litecr.source_assertion a JOIN litecr.source_record s USING (source_record_id) WHERE s.registry_id=$1 AND s.organization_id=$2 AND s.source_system_id=$3 AND s.source_record_key=ANY($4::text[])) AS assertions, \
          (SELECT COUNT(*) FROM litecr.source_assertion a JOIN litecr.source_record s USING (source_record_id) WHERE s.registry_id=$1 AND s.organization_id=$2 AND s.source_system_id=$3 AND s.source_record_key=ANY($5::text[]) AND a.withdrawn_by_manifest=$12) AS withdrawn_target_assertions, \
          (SELECT COUNT(*) FROM litecr.person p WHERE p.registry_id=$1 AND p.organization_id=$2 AND p.public_id=ANY($7::text[])) AS people, \
          (SELECT COUNT(*) FROM litecr.person_alias a WHERE a.registry_id=$1 AND a.organization_id=$2 AND a.alias_value=ANY($8::text[])) AS aliases, \
          (SELECT COUNT(*) FROM litecr.person_identifier i WHERE i.registry_id=$1 AND i.organization_id=$2 AND i.authority='urn:litecr:test:clinic' AND i.identifier_type='MRN' AND i.identifier_value=ANY($9::text[])) AS identifiers, \
          (SELECT COUNT(*) FROM litecr.person_identifier i WHERE i.registry_id=$1 AND i.organization_id=$2 AND i.authority='urn:litecr:test:clinic' AND i.identifier_type='MRN' AND i.identifier_value=ANY($10::text[])) AS target_identifiers, \
          (SELECT COUNT(*) FROM litecr.person_identifier i WHERE i.registry_id=$1 AND i.organization_id=$2 AND i.authority='urn:litecr:test:clinic' AND i.identifier_type='MRN' AND i.identifier_value=ANY($11::text[])) AS control_identifiers, \
          (SELECT COUNT(*) FROM litecr.mutation_audit a WHERE a.registry_id=$1 AND a.organization_id=$2 AND a.action='registry.source_record.accept' AND a.resource_ref IN (SELECT s.source_record_id::text FROM litecr.source_record s WHERE s.registry_id=$1 AND s.organization_id=$2 AND s.source_system_id=$3 AND s.source_record_key=ANY($4::text[]))) AS imported_audits, \
          (SELECT COUNT(*) FROM litecr.mutation_audit a WHERE a.registry_id=$1 AND a.organization_id=$2 AND a.action='migration.source.withdraw' AND a.resource_ref IN (SELECT s.source_record_id::text FROM litecr.source_record s WHERE s.registry_id=$1 AND s.organization_id=$2 AND s.source_system_id=$3 AND s.source_record_key=ANY($5::text[]))) AS withdrawal_audits, \
          (SELECT COUNT(*) FROM litecr.outbox_event e WHERE e.registry_id=$1 AND e.organization_id=$2 AND e.event_type='registry.source_record.accepted' AND e.aggregate_id IN (SELECT s.source_record_id FROM litecr.source_record s WHERE s.registry_id=$1 AND s.organization_id=$2 AND s.source_system_id=$3 AND s.source_record_key=ANY($4::text[]))) AS imported_events, \
          (SELECT COUNT(*) FROM litecr.outbox_event e WHERE e.registry_id=$1 AND e.organization_id=$2 AND e.event_type='registry.source_record.withdrawn' AND e.aggregate_id IN (SELECT s.source_record_id FROM litecr.source_record s WHERE s.registry_id=$1 AND s.organization_id=$2 AND s.source_system_id=$3 AND s.source_record_key=ANY($5::text[]))) AS withdrawal_events, \
          (SELECT COUNT(*) FROM litecr.migration_reverse_receipt r WHERE r.manifest_sha256=$12 AND r.registry_id=$1 AND r.organization_id=$2) AS reverse_receipts",
    )
    .bind(registry_id)
    .bind(organization_id)
    .bind(SOURCE_SYSTEM)
    .bind(all_keys)
    .bind(target_keys)
    .bind(control_keys)
    .bind(public_ids)
    .bind(aliases)
    .bind(identifiers)
    .bind(target_identifiers)
    .bind(control_identifiers)
    .bind(target.manifest_hash.to_vec())
    .fetch_one(pool)
    .await
    .map_err(|_| "independent SQL reconciliation snapshot reads exact synthetic scope".to_owned())
}

async fn assert_retained_aliases(
    pool: &sqlx::PgPool,
    registry_id: &str,
    organization_id: &str,
    target: &SyntheticRun,
    control: &SyntheticRun,
) -> Result<(), String> {
    let expected: Vec<String> = target
        .records
        .iter()
        .chain(control.records.iter())
        .flat_map(|record| {
            [
                record.public_id.clone(),
                record.cruid.clone(),
                record.alias.clone(),
            ]
        })
        .collect();
    let actual: Vec<String> = sqlx::query_scalar(
        "SELECT alias_value FROM litecr.person_alias WHERE registry_id=$1 AND organization_id=$2 AND alias_value=ANY($3::text[]) ORDER BY alias_value",
    )
    .bind(registry_id)
    .bind(organization_id)
    .bind(&expected)
    .fetch_all(pool)
    .await
    .map_err(|_| "independent SQL query reads retained synthetic aliases".to_owned())?;
    let mut sorted_expected = expected;
    sorted_expected.sort();
    ensure(
        actual == sorted_expected,
        "all imported and unrelated aliases remain exact after source-only reverse",
    )
}

async fn assert_partial_date_and_script(
    pool: &sqlx::PgPool,
    registry_id: &str,
    organization_id: &str,
    source_key: &str,
) -> Result<(), String> {
    let row = sqlx::query(
        "SELECT a.original_value, a.language_tag, a.script, a.field_path \
         FROM litecr.source_assertion a JOIN litecr.source_record s USING (source_record_id) \
         WHERE s.registry_id=$1 AND s.organization_id=$2 AND s.source_record_key=$3 \
           AND s.source_system_id=$4 AND a.field_path IN ('name.original','birthDate') \
         ORDER BY a.field_path",
    )
    .bind(registry_id)
    .bind(organization_id)
    .bind(source_key)
    .bind(SOURCE_SYSTEM)
    .fetch_all(pool)
    .await
    .map_err(|_| "independent SQL reads synthetic original-script assertions".to_owned())?;
    ensure(row.len() == 2, "two source assertions remain recorded")?;
    let name = row
        .iter()
        .find(|item| {
            item.try_get::<String, _>("field_path").ok().as_deref() == Some("name.original")
        })
        .ok_or_else(|| "original-script name assertion remains recorded".to_owned())?;
    ensure(
        name.try_get::<String, _>("original_value")
            .ok()
            .is_some_and(|value| value.starts_with("ຊື່ທົດສອບ "))
            && name
                .try_get::<Option<String>, _>("language_tag")
                .ok()
                .flatten()
                .as_deref()
                == Some("lo")
            && name
                .try_get::<Option<String>, _>("script")
                .ok()
                .flatten()
                .as_deref()
                == Some("Lao"),
        "source assertion preserves its Lao original script and language tag",
    )?;
    let birth_date = row
        .iter()
        .find(|item| item.try_get::<String, _>("field_path").ok().as_deref() == Some("birthDate"))
        .ok_or_else(|| "partial birth-date assertion remains recorded".to_owned())?;
    ensure(
        birth_date
            .try_get::<String, _>("original_value")
            .ok()
            .as_deref()
            == Some("1984-06"),
        "partial-date source value remains unchanged",
    )
}

async fn run_scenario(database_url: String, key: String) -> Result<(), String> {
    let pool = connect(&database_url, 10)
        .await
        .map_err(|_| "synthetic-only PostgreSQL test database is reachable".to_owned())?;
    migrate(&pool)
        .await
        .map_err(|_| "synthetic-only database schema is current".to_owned())?;

    let registry_id = format!("api-migration-reg-{key}");
    let organization_id = format!("api-migration-org-{key}");
    let wrong_registry_id = format!("api-migration-other-reg-{key}");
    let wrong_organization_id = format!("api-migration-other-org-{key}");
    let subject = format!("api-migration-client-{key}");
    let read_subject = format!("api-migration-reader-{key}");
    let wrong_subject = format!("api-migration-other-client-{key}");
    let target = synthetic_run(
        &format!("target-{key}"),
        &registry_id,
        &organization_id,
        vec![
            synthetic_record("target", &format!("first-{key}"))?,
            synthetic_record("target", &format!("second-{key}"))?,
        ],
    );
    let control = synthetic_run(
        &format!("unrelated-{key}"),
        &registry_id,
        &organization_id,
        vec![synthetic_record("unrelated", &key)?],
    );

    let credential =
        provision_integration_client(&pool, &subject, &registry_id, &organization_id).await?;
    let read_credential =
        provision_migration_reader(&pool, &read_subject, &registry_id, &organization_id).await?;
    let wrong_credential = provision_integration_client(
        &pool,
        &wrong_subject,
        &wrong_registry_id,
        &wrong_organization_id,
    )
    .await?;
    let api = serve_migration_api(
        pool.clone(),
        &registry_id,
        &organization_id,
        &subject,
        &wrong_registry_id,
        &wrong_organization_id,
        &wrong_subject,
    )
    .await?;
    let client = reqwest::Client::builder()
        .no_proxy()
        .build()
        .map_err(|_| "local test HTTP client builds without proxy inheritance".to_owned())?;

    let batch_1 = batch_body(&target, 1, 0);
    let batch_2 = batch_body(&target, 2, 1);
    let control_batch = batch_body(&control, 1, 0);

    let (unauthenticated_status, _) = post_json(
        &client,
        &api.base_url,
        "/api/v1/migrations/batches/validate",
        None,
        &batch_1,
    )
    .await?;
    ensure(
        unauthenticated_status == reqwest::StatusCode::UNAUTHORIZED,
        "protected migration validation rejects requests without credentials",
    )?;
    let (wrong_scope_status, _) = post_json(
        &client,
        &api.base_url,
        "/api/v1/migrations/batches/validate",
        Some(&wrong_credential),
        &batch_1,
    )
    .await?;
    ensure(
        wrong_scope_status == reqwest::StatusCode::FORBIDDEN,
        "database-backed integration credential cannot validate a batch outside its grant scope",
    )?;

    let before = snapshot(&pool, &registry_id, &organization_id, &target, &control).await?;
    ensure(
        before == Snapshot::default(),
        "unique synthetic registry scope starts with no matching Registry or migration rows",
    )?;

    for body in [&batch_1, &batch_2, &control_batch] {
        let (status, _) = post_json(
            &client,
            &api.base_url,
            "/api/v1/migrations/batches/validate",
            Some(&credential),
            body,
        )
        .await?;
        ensure(
            status == reqwest::StatusCode::NO_CONTENT,
            "authorized no-write batch validation accepts supported synthetic rows",
        )?;
    }
    let after_validation =
        snapshot(&pool, &registry_id, &organization_id, &target, &control).await?;
    ensure(
        after_validation == before,
        "no-write validation leaves source, person, alias, identifier, audit, outbox, and reverse-receipt counts unchanged",
    )?;

    let (reader_validate_status, _) = post_json(
        &client,
        &api.base_url,
        "/api/v1/migrations/batches/validate",
        Some(&read_credential),
        &batch_1,
    )
    .await?;
    let (reader_apply_status, _) = post_json(
        &client,
        &api.base_url,
        "/api/v1/migrations/batches",
        Some(&read_credential),
        &batch_1,
    )
    .await?;
    ensure(
        reader_validate_status == reqwest::StatusCode::NO_CONTENT
            && reader_apply_status == reqwest::StatusCode::FORBIDDEN
            && snapshot(&pool, &registry_id, &organization_id, &target, &control).await? == before,
        "migration:read permits no-write package validation but cannot commit a batch",
    )?;

    let (status, first_response) = post_json(
        &client,
        &api.base_url,
        "/api/v1/migrations/batches",
        Some(&credential),
        &batch_1,
    )
    .await?;
    ensure(
        status == reqwest::StatusCode::ACCEPTED,
        "first authorized batch commits",
    )?;
    ensure(
        first_response["data"]["state"] == "importing"
            && first_response["data"]["committed_batch_sequence"] == 1
            && first_response["data"]["committed_record_count"] == 1,
        "first batch response records the resumable partial checkpoint",
    )?;
    let partial = snapshot(&pool, &registry_id, &organization_id, &target, &control).await?;
    ensure(
        partial.source_records == 1
            && partial.active_sources == 1
            && partial.source_versions == 1
            && partial.assertions == 2
            && partial.people == 1
            && partial.aliases == 3
            && partial.identifiers == 1
            && partial.imported_audits == 1
            && partial.imported_events == 1
            && partial.withdrawal_audits == 0
            && partial.withdrawal_events == 0
            && partial.reverse_receipts == 0,
        "independent SQL sees exactly the first imported source and its durable Registry effects",
    )?;

    let (reader_checkpoint_status, reader_checkpoint) = post_json(
        &client,
        &api.base_url,
        "/api/v1/migrations/checkpoint",
        Some(&read_credential),
        &json!({"run":target.body}),
    )
    .await?;
    ensure(
        reader_checkpoint_status == reqwest::StatusCode::OK
            && reader_checkpoint["data"]["state"] == "importing"
            && reader_checkpoint["data"]["committed_batch_sequence"] == 1,
        "scoped migration:read can inspect the current checkpoint",
    )?;
    let (reader_preflight_status, reader_preflight) = post_json(
        &client,
        &api.base_url,
        "/api/v1/migrations/reverse/preflight",
        Some(&read_credential),
        &json!({
            "manifest_sha256": target.manifest_hex,
            "registry_id": registry_id,
            "organization_id": organization_id
        }),
    )
    .await?;
    ensure(
        reader_preflight_status == reqwest::StatusCode::OK
            && reader_preflight["data"]["eligible"] == true,
        "scoped migration:read can inspect that the partial import is eligible for source-only reversal",
    )?;
    let (reader_reverse_status, _) = post_json(
        &client,
        &api.base_url,
        "/api/v1/migrations/reverse",
        Some(&read_credential),
        &json!({
            "manifest_sha256": target.manifest_hex,
            "confirm_manifest_sha256": target.manifest_hex,
            "registry_id": registry_id,
            "organization_id": organization_id,
            "reason": "read-only operator cannot reverse source data"
        }),
    )
    .await?;
    ensure(
        reader_reverse_status == reqwest::StatusCode::FORBIDDEN,
        "migration:read cannot reverse imported sources",
    )?;

    let imported = &target.records[0];
    let (fhir_status, fhir_bundle) = get_fhir_identifier(
        &client,
        &api.base_url,
        "urn:litecr:test:clinic",
        &imported.identifier,
        &credential,
    )
    .await?;
    ensure(
        fhir_status == reqwest::StatusCode::OK
            && fhir_bundle["resourceType"] == "Bundle"
            && fhir_bundle["total"] == 1
            && fhir_bundle["entry"][0]["resource"]["resourceType"] == "Patient"
            && fhir_bundle["entry"][0]["resource"]["id"] == imported.public_id
            && fhir_bundle["entry"][0]["resource"]
                .get("language")
                .is_none()
            && fhir_bundle["entry"][0]["resource"]["birthDate"] == "1984-06"
            && fhir_bundle["entry"][0]["resource"]["name"][0]["extension"][0]["url"]
                == "http://hl7.org/fhir/StructureDefinition/language"
            && fhir_bundle["entry"][0]["resource"]["name"][0]["extension"][0]["valueCode"] == "lo"
            && fhir_bundle["entry"][0]["resource"]["name"][0]["extension"][1]["url"]
                == "urn:litecr:fhir:StructureDefinition/human-name-script"
            && fhir_bundle["entry"][0]["resource"]["name"][0]["extension"][1]["valueString"]
                == "Lao"
            && fhir_bundle["entry"][0]["resource"]["identifier"][0]["system"]
                == "urn:litecr:test:clinic"
            && fhir_bundle["entry"][0]["resource"]["identifier"][0]["value"] == imported.identifier,
        &format!(
            "authenticated FHIR exact identifier search finds the just-imported source and projects its authority, value, and public ID: status={fhir_status}, body={fhir_bundle}"
        ),
    )?;

    let public_id_search = json!({
        "registry_id": registry_id,
        "organization_id": organization_id,
        "public_id_or_alias": imported.public_id,
        "limit": 20
    });
    let (public_search_status, public_search) = post_json(
        &client,
        &api.base_url,
        "/api/v1/person-search",
        Some(&credential),
        &public_id_search,
    )
    .await?;
    ensure(
        public_search_status == reqwest::StatusCode::OK
            && public_search["data"].as_array().is_some_and(|people| {
                people.len() == 1 && people[0]["public_id"] == imported.public_id
            }),
        "authenticated public-ID search finds exactly the imported person",
    )?;

    let (wrong_client_status, wrong_client_bundle) = get_fhir_identifier(
        &client,
        &api.base_url,
        "urn:litecr:test:clinic",
        &imported.identifier,
        &wrong_credential,
    )
    .await?;
    ensure(
        wrong_client_status == reqwest::StatusCode::OK
            && wrong_client_bundle["resourceType"] == "Bundle"
            && wrong_client_bundle["total"] == 0
            && wrong_client_bundle["entry"]
                .as_array()
                .is_some_and(Vec::is_empty),
        "client bound to a different registry and organization receives an empty FHIR search without imported values",
    )?;

    let cross_scope_search = json!({
        "registry_id": wrong_registry_id,
        "organization_id": wrong_organization_id,
        "public_id_or_alias": imported.public_id,
        "limit": 20
    });
    let (cross_scope_status, cross_scope_body) = post_json(
        &client,
        &api.base_url,
        "/api/v1/person-search",
        Some(&credential),
        &cross_scope_search,
    )
    .await?;
    ensure(
        cross_scope_status == reqwest::StatusCode::FORBIDDEN
            && cross_scope_body["error"]["code"] == "ACCESS_DENIED"
            && !cross_scope_body.to_string().contains(&imported.identifier)
            && !cross_scope_body.to_string().contains(&imported.public_id),
        "client cannot move public-ID search into another registry or organization and receives no imported values",
    )?;
    assert_partial_date_and_script(
        &pool,
        &registry_id,
        &organization_id,
        &target.records[0].source_key,
    )
    .await?;

    let checkpoint_request = json!({"run": target.body});
    let (status, checkpoint) = post_json(
        &client,
        &api.base_url,
        "/api/v1/migrations/checkpoint",
        Some(&credential),
        &checkpoint_request,
    )
    .await?;
    ensure(
        status == reqwest::StatusCode::OK
            && checkpoint["data"]["state"] == "importing"
            && checkpoint["data"]["committed_batch_sequence"] == 1
            && checkpoint["data"]["committed_record_count"] == 1,
        "authenticated checkpoint reports exact partial progress before resume",
    )?;

    // A later, separately manifested write in the same organization proves
    // reversing the target manifest does not restore a database snapshot.
    let (status, _) = post_json(
        &client,
        &api.base_url,
        "/api/v1/migrations/batches",
        Some(&credential),
        &control_batch,
    )
    .await?;
    ensure(
        status == reqwest::StatusCode::ACCEPTED,
        "unrelated scoped source write commits",
    )?;

    let (status, second_response) = post_json(
        &client,
        &api.base_url,
        "/api/v1/migrations/batches",
        Some(&credential),
        &batch_2,
    )
    .await?;
    ensure(
        status == reqwest::StatusCode::ACCEPTED,
        "next target batch resumes the partial manifest",
    )?;
    ensure(
        second_response["data"]["state"] == "completed"
            && second_response["data"]["committed_batch_sequence"] == 2
            && second_response["data"]["committed_record_count"] == 2,
        "resumed batch advances target manifest to completed",
    )?;
    let completed = snapshot(&pool, &registry_id, &organization_id, &target, &control).await?;
    ensure(
        completed.source_records == 3
            && completed.active_sources == 3
            && completed.source_versions == 3
            && completed.assertions == 6
            && completed.people == 3
            && completed.aliases == 9
            && completed.identifiers == 3
            && completed.target_identifiers == 2
            && completed.control_identifiers == 1
            && completed.imported_audits == 3
            && completed.imported_events == 3
            && completed.withdrawal_audits == 0
            && completed.withdrawal_events == 0
            && completed.reverse_receipts == 0,
        "independent SQL reconciles both target batches plus the later unrelated manifest",
    )?;

    let (status, retry_response) = post_json(
        &client,
        &api.base_url,
        "/api/v1/migrations/batches",
        Some(&credential),
        &batch_2,
    )
    .await?;
    ensure(
        status == reqwest::StatusCode::ACCEPTED,
        "duplicate batch retry returns success",
    )?;
    ensure(
        retry_response == second_response,
        "duplicate batch retry returns the original API receipt",
    )?;
    let after_retry = snapshot(&pool, &registry_id, &organization_id, &target, &control).await?;
    ensure(
        after_retry == completed,
        "duplicate batch retry does not duplicate source, person, alias, identifier, audit, or outbox effects",
    )?;

    let reverse_request = json!({
        "manifest_sha256": target.manifest_hex,
        "confirm_manifest_sha256": target.manifest_hex,
        "registry_id": registry_id,
        "organization_id": organization_id,
        "reason": "synthetic API reconciliation reverse"
    });
    let (status, reverse_response) = post_json(
        &client,
        &api.base_url,
        "/api/v1/migrations/reverse",
        Some(&credential),
        &reverse_request,
    )
    .await?;
    ensure(
        status == reqwest::StatusCode::OK,
        "confirmed manifest source-only reverse succeeds",
    )?;
    ensure(
        reverse_response["data"]["state"] == "reversed"
            && reverse_response["data"]["withdrawn_record_count"] == 2
            && reverse_response["data"]["detached_identifier_count"] == 2
            && reverse_response["data"]["retained_alias_count"] == 6,
        "reverse receipt describes only the target manifest effects",
    )?;

    let after_reverse = snapshot(&pool, &registry_id, &organization_id, &target, &control).await?;
    ensure(
        after_reverse.source_records == 3
            && after_reverse.active_sources == 1
            && after_reverse.withdrawn_target_sources == 2
            && after_reverse.source_versions == 3
            && after_reverse.withdrawn_target_versions == 2
            && after_reverse.assertions == 6
            && after_reverse.withdrawn_target_assertions == 4
            && after_reverse.people == 3
            && after_reverse.aliases == 9
            && after_reverse.identifiers == 1
            && after_reverse.target_identifiers == 0
            && after_reverse.control_identifiers == 1
            && after_reverse.imported_audits == 3
            && after_reverse.withdrawal_audits == 2
            && after_reverse.imported_events == 3
            && after_reverse.withdrawal_events == 2
            && after_reverse.reverse_receipts == 1,
        "independent SQL proves target history/people/aliases remain, target identifiers detach, and unrelated scoped write survives",
    )?;
    assert_retained_aliases(&pool, &registry_id, &organization_id, &target, &control).await?;
    assert_partial_date_and_script(
        &pool,
        &registry_id,
        &organization_id,
        &target.records[0].source_key,
    )
    .await?;

    let (status, reverse_retry) = post_json(
        &client,
        &api.base_url,
        "/api/v1/migrations/reverse",
        Some(&credential),
        &reverse_request,
    )
    .await?;
    ensure(
        status == reqwest::StatusCode::OK && reverse_retry == reverse_response,
        "confirmed reverse retry returns the original durable receipt",
    )?;
    let after_reverse_retry =
        snapshot(&pool, &registry_id, &organization_id, &target, &control).await?;
    ensure(
        after_reverse_retry == after_reverse,
        "reverse retry does not duplicate receipt, audit, or outbox effects",
    )?;

    let cli_record = synthetic_record("cli", &key)?;
    let cli_fixture = create_cli_fixture(&key, &registry_id, &organization_id, &credential)?;
    let cli_common = vec![
        "--manifest".to_owned(),
        cli_fixture.manifest.to_string_lossy().into_owned(),
        "--credential-file".to_owned(),
        cli_fixture.credential.to_string_lossy().into_owned(),
        "--api-base-url".to_owned(),
        api.base_url.clone(),
    ];
    let mut apply_args = vec![
        "migration".to_owned(),
        "apply".to_owned(),
        "--manifest".to_owned(),
        cli_fixture.manifest.to_string_lossy().into_owned(),
        "--stream".to_owned(),
        cli_fixture.stream.to_string_lossy().into_owned(),
        "--stage-dir".to_owned(),
        cli_fixture.stage.to_string_lossy().into_owned(),
        "--credential-file".to_owned(),
        cli_fixture.credential.to_string_lossy().into_owned(),
        "--api-base-url".to_owned(),
        api.base_url.clone(),
        "--confirm-manifest-sha256".to_owned(),
        cli_fixture.manifest_sha256.clone(),
    ];
    let cli_apply = run_litecr_cli(std::mem::take(&mut apply_args)).await?;
    ensure(
        cli_apply.status.success(),
        &format!(
            "real litecr migration apply CLI succeeds against protected API (status={}): stdout={} stderr={}",
            cli_apply.status,
            String::from_utf8_lossy(&cli_apply.stdout),
            String::from_utf8_lossy(&cli_apply.stderr),
        ),
    )?;
    let cli_source_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM litecr.source_record WHERE registry_id=$1 AND organization_id=$2 \
         AND source_system_id=$3 AND source_record_key=$4",
    )
    .bind(&registry_id)
    .bind(&organization_id)
    .bind(SOURCE_SYSTEM)
    .bind(&cli_record.source_key)
    .fetch_one(&pool)
    .await
    .map_err(|_| "independent SQL verifies the real CLI source effect".to_owned())?;
    ensure(
        cli_source_count == 1,
        "real litecr migration apply CLI creates exactly one scoped synthetic source",
    )?;

    let mut status_args = vec!["migration".to_owned(), "status".to_owned()];
    status_args.extend(cli_common.clone());
    let cli_status = run_litecr_cli(status_args).await?;
    ensure(
        cli_status.status.success()
            && String::from_utf8_lossy(&cli_status.stdout).contains("migration status: completed"),
        "real litecr migration status CLI reads the completed checkpoint",
    )?;

    let mut check_args = vec!["migration".to_owned(), "reverse-check".to_owned()];
    check_args.extend(cli_common.clone());
    let cli_reverse_check = run_litecr_cli(check_args).await?;
    ensure(
        cli_reverse_check.status.success()
            && String::from_utf8_lossy(&cli_reverse_check.stdout)
                .contains("eligible at query time"),
        "real litecr migration reverse-check CLI reports current eligibility",
    )?;

    let mut reverse_args = vec![
        "migration".to_owned(),
        "reverse".to_owned(),
        "--reason".to_owned(),
        "synthetic CLI reconciliation reverse".to_owned(),
        "--confirm-manifest-sha256".to_owned(),
        cli_fixture.manifest_sha256.clone(),
    ];
    reverse_args.extend(cli_common);
    let cli_reverse = run_litecr_cli(reverse_args).await?;
    ensure(
        cli_reverse.status.success()
            && String::from_utf8_lossy(&cli_reverse.stdout)
                .contains("migration reversed: reversed"),
        "real litecr migration reverse CLI performs confirmed source-only reverse",
    )?;
    let cli_withdrawn: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM litecr.source_record_version v \
         JOIN litecr.source_record s USING (source_record_id) \
         WHERE s.registry_id=$1 AND s.organization_id=$2 AND s.source_record_key=$3 \
         AND v.withdrawn_by_manifest=decode($4,'hex')",
    )
    .bind(&registry_id)
    .bind(&organization_id)
    .bind(&cli_record.source_key)
    .bind(&cli_fixture.manifest_sha256)
    .fetch_one(&pool)
    .await
    .map_err(|_| "independent SQL verifies the real CLI reverse effect".to_owned())?;
    ensure(
        cli_withdrawn == 1,
        "real litecr migration reverse CLI withdraws only its journaled synthetic source",
    )?;

    api.stop().await?;
    pool.close().await;
    Ok(())
}

async fn cleanup(database_url: &str, key: &str) {
    let Ok(pool) = connect(database_url, 6).await else {
        return;
    };
    let registry_id = format!("api-migration-reg-{key}");
    let organization_id = format!("api-migration-org-{key}");
    let wrong_registry_id = format!("api-migration-other-reg-{key}");
    let wrong_organization_id = format!("api-migration-other-org-{key}");
    let subjects = [
        format!("api-migration-client-{key}"),
        format!("api-migration-reader-{key}"),
        format!("api-migration-other-client-{key}"),
    ];

    let manifests: Vec<Vec<u8>> = sqlx::query_scalar(
        "SELECT manifest_sha256 FROM litecr.migration_checkpoint WHERE \
         (registry_id=$1 AND organization_id=$2) OR (registry_id=$3 AND organization_id=$4)",
    )
    .bind(&registry_id)
    .bind(&organization_id)
    .bind(&wrong_registry_id)
    .bind(&wrong_organization_id)
    .fetch_all(&pool)
    .await
    .unwrap_or_default();
    if !manifests.is_empty() {
        sqlx::query(
            "DELETE FROM litecr.migration_reverse_receipt WHERE manifest_sha256=ANY($1::bytea[])",
        )
        .bind(&manifests)
        .execute(&pool)
        .await
        .ok();
        sqlx::query("DELETE FROM litecr.migration_journal WHERE manifest_sha256=ANY($1::bytea[])")
            .bind(&manifests)
            .execute(&pool)
            .await
            .ok();
        sqlx::query("DELETE FROM litecr.migration_effect WHERE manifest_sha256=ANY($1::bytea[])")
            .bind(&manifests)
            .execute(&pool)
            .await
            .ok();
        sqlx::query("DELETE FROM litecr.migration_batch WHERE manifest_sha256=ANY($1::bytea[])")
            .bind(&manifests)
            .execute(&pool)
            .await
            .ok();
        sqlx::query(
            "DELETE FROM litecr.migration_checkpoint WHERE manifest_sha256=ANY($1::bytea[])",
        )
        .bind(&manifests)
        .execute(&pool)
        .await
        .ok();
    }

    let source_ids: Vec<String> = sqlx::query_scalar(
        "SELECT source_record_id::text FROM litecr.source_record WHERE \
         (registry_id=$1 AND organization_id=$2) OR (registry_id=$3 AND organization_id=$4)",
    )
    .bind(&registry_id)
    .bind(&organization_id)
    .bind(&wrong_registry_id)
    .bind(&wrong_organization_id)
    .fetch_all(&pool)
    .await
    .unwrap_or_default();
    sqlx::query("DELETE FROM litecr.registry_idempotency WHERE actor_id=ANY($1::text[]) AND registry_id=ANY($2::text[])")
        .bind(&subjects)
        .bind([&registry_id, &wrong_registry_id])
        .execute(&pool)
        .await
        .ok();
    sqlx::query("DELETE FROM litecr.outbox_event WHERE (registry_id=$1 AND organization_id=$2) OR (registry_id=$3 AND organization_id=$4)")
        .bind(&registry_id)
        .bind(&organization_id)
        .bind(&wrong_registry_id)
        .bind(&wrong_organization_id)
        .execute(&pool)
        .await
        .ok();
    sqlx::query("DELETE FROM litecr.mutation_audit WHERE (registry_id=$1 AND organization_id=$2) OR (registry_id=$3 AND organization_id=$4)")
        .bind(&registry_id)
        .bind(&organization_id)
        .bind(&wrong_registry_id)
        .bind(&wrong_organization_id)
        .execute(&pool)
        .await
        .ok();
    sqlx::query("DELETE FROM litecr.access_integration_credential WHERE issuer_url='' AND external_subject=ANY($1::text[])")
        .bind(&subjects)
        .execute(&pool)
        .await
        .ok();
    sqlx::query("DELETE FROM litecr.access_audit WHERE actor_kind='integration_client' AND actor_issuer_url='' AND actor_id=ANY($1::text[])")
        .bind(&subjects)
        .execute(&pool)
        .await
        .ok();

    if !source_ids.is_empty() {
        sqlx::query("DELETE FROM litecr.source_identifier_assertion WHERE source_record_id::text=ANY($1::text[])")
            .bind(&source_ids)
            .execute(&pool)
            .await
            .ok();
        sqlx::query(
            "DELETE FROM litecr.person_identifier WHERE source_record_id::text=ANY($1::text[])",
        )
        .bind(&source_ids)
        .execute(&pool)
        .await
        .ok();
        sqlx::query(
            "DELETE FROM litecr.source_assertion WHERE source_record_id::text=ANY($1::text[])",
        )
        .bind(&source_ids)
        .execute(&pool)
        .await
        .ok();
        sqlx::query(
            "DELETE FROM litecr.source_record_version WHERE source_record_id::text=ANY($1::text[])",
        )
        .bind(&source_ids)
        .execute(&pool)
        .await
        .ok();
        sqlx::query(
            "DELETE FROM litecr.source_record WHERE source_record_id::text=ANY($1::text[])",
        )
        .bind(&source_ids)
        .execute(&pool)
        .await
        .ok();
    }
    let public_ids: Vec<String> = [
        "target-public-first",
        "target-public-second",
        "unrelated-public",
    ]
    .into_iter()
    .map(|name| format!("{name}-{key}"))
    .collect();
    let person_ids: Vec<String> = sqlx::query_scalar(
        "SELECT person_id::text FROM litecr.person WHERE registry_id=$1 AND organization_id=$2 AND public_id=ANY($3::text[])",
    )
    .bind(&registry_id)
    .bind(&organization_id)
    .bind(&public_ids)
    .fetch_all(&pool)
    .await
    .unwrap_or_default();
    if !person_ids.is_empty() {
        sqlx::query("DELETE FROM litecr.person_alias WHERE person_id::text=ANY($1::text[])")
            .bind(&person_ids)
            .execute(&pool)
            .await
            .ok();
        sqlx::query("DELETE FROM litecr.person WHERE person_id::text=ANY($1::text[])")
            .bind(&person_ids)
            .execute(&pool)
            .await
            .ok();
    }
    sqlx::query("DELETE FROM litecr.access_grant WHERE actor_kind='integration_client' AND issuer_url='' AND external_subject=ANY($1::text[])")
        .bind(&subjects)
        .execute(&pool)
        .await
        .ok();
    sqlx::query("DELETE FROM litecr.access_identity WHERE actor_kind='integration_client' AND issuer_url='' AND external_subject=ANY($1::text[])")
        .bind(&subjects)
        .execute(&pool)
        .await
        .ok();
    pool.close().await;
}

fn ensure(condition: bool, message: &str) -> Result<(), String> {
    condition.then_some(()).ok_or_else(|| message.to_owned())
}

struct UnusedOidcProvider;

impl OidcProvider for UnusedOidcProvider {
    fn begin_authorization<'life0, 'async_trait>(
        &'life0 self,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<OidcAuthorization, OidcProviderError>> + Send + 'async_trait,
        >,
    >
    where
        'life0: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async { Err(OidcProviderError::Unavailable) })
    }

    fn exchange_code<'life0, 'life1, 'life2, 'async_trait>(
        &'life0 self,
        _code: &'life1 str,
        _flow: &'life2 OidcFlow,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<OidcIdentityClaims, OidcProviderError>>
                + Send
                + 'async_trait,
        >,
    >
    where
        'life0: 'async_trait,
        'life1: 'async_trait,
        'life2: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async { Err(OidcProviderError::Unavailable) })
    }
}

#[tokio::test]
async fn protected_migration_api_reconciles_resume_retry_and_source_only_reverse() {
    let database_url = std::env::var(TEST_DATABASE_ENV)
        .expect("set LITECR_TEST_DATABASE_URL to the serialized synthetic-only PostgreSQL DB");
    let key = suffix();
    let result = tokio::spawn(run_scenario(database_url.clone(), key.clone())).await;
    cleanup(&database_url, &key).await;
    assert!(
        result.is_ok(),
        "synthetic API reconciliation task joins: {result:?}"
    );
    result
        .unwrap()
        .expect("protected migration API PostgreSQL reconciliation scenario");
}
