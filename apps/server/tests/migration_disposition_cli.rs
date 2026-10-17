//! Synthetic end-to-end coverage for the offline Python stager and protected
//! `litecr migration disposition` CLI. The fixture contains no real records.

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
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use std::{
    fs,
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    process::{Command, Output},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::{net::TcpListener, sync::oneshot, task::JoinHandle};

const DB_ENV: &str = "LITECR_TEST_DATABASE_URL";
const SOURCE: &str = "synthetic-disposition-e2e";

struct UnusedOidcProvider;

#[cfg(unix)]
fn make_private(path: &Path, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn make_private(_path: &Path, _mode: u32) -> std::io::Result<()> {
    Ok(())
}

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

struct ApiHandle {
    url: String,
    stop: oneshot::Sender<()>,
    task: JoinHandle<std::io::Result<()>>,
}

impl ApiHandle {
    async fn stop(self) -> Result<(), String> {
        let _ = self.stop.send(());
        self.task
            .await
            .map_err(|_| "protected test API task joins".to_owned())?
            .map_err(|_| "protected test API stops cleanly".to_owned())
    }
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from(HEX[usize::from(byte >> 4)]));
        out.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    out
}

fn project_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("server crate is two levels below the workspace root")
        .to_path_buf()
}

fn remove_owned_temp_fixture(root: &Path) {
    let Some(parent) = root.parent() else {
        return;
    };
    let Ok(parent) = parent.canonicalize() else {
        return;
    };
    let Ok(temp_dir) = std::env::temp_dir().canonicalize() else {
        return;
    };
    let Some(name) = root.file_name().and_then(|name| name.to_str()) else {
        return;
    };
    let owned = fs::read_to_string(root.join(".owner"))
        .is_ok_and(|marker| marker == "litecr-disposition-e2e-v1");
    if parent == temp_dir && name.starts_with("litecr-disposition-e2e-") && owned {
        let _ = fs::remove_dir_all(root);
    }
}

fn safe_synthetic_database(url: &str) -> bool {
    let name = url
        .split('?')
        .next()
        .and_then(|u| u.rsplit('/').next())
        .unwrap_or_default();
    let name = name.to_ascii_lowercase();
    name != "litecr_access_test"
        && ["test", "synthetic", "fixture"]
            .iter()
            .any(|marker| name.contains(marker))
}

async fn provision(
    pool: &PgPool,
    subject: &str,
    scope_key: &str,
    permission: &str,
) -> Result<String, String> {
    sqlx::query(
        "INSERT INTO litecr.access_identity (actor_kind, issuer_url, external_subject) \
         VALUES ('integration_client', '', $1)",
    )
    .bind(subject)
    .execute(pool)
    .await
    .map_err(|_| "unique synthetic integration identity is inserted".to_owned())?;
    sqlx::query(
        "INSERT INTO litecr.access_grant \
         (actor_kind, issuer_url, external_subject, registry_id, organization_id, permission_name) \
         VALUES ('integration_client', '', $1, $2, $3, $4)",
    )
    .bind(subject)
    .bind(format!("disposition-reg-{scope_key}"))
    .bind(format!("disposition-org-{scope_key}"))
    .bind(permission)
    .execute(pool)
    .await
    .map_err(|_| "synthetic scoped migration grant is inserted".to_owned())?;
    let directory = PostgresAccessDirectory::new(pool.clone());
    let sessions = PostgresAccessSessionRepository::new(pool.clone(), directory);
    sessions
        .issue_integration_credential(None, subject, Some(600))
        .await
        .map_err(|_| "ephemeral synthetic integration credential is issued".to_owned())
}

async fn start_api(pool: PgPool) -> Result<ApiHandle, String> {
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
        PostgresMigrationExecutor::new(pool),
    ));
    let routes = litecr_api::migration::router(Arc::new(migration));
    let router = litecr_api::access::protected_routes(
        routes,
        Arc::new(access),
        AccessApiConfig {
            expected_origin: "http://127.0.0.1".into(),
            secure_cookies: false,
            environment: AccessEnvironment::Development,
            session_lifetime_seconds: 600,
            flow_lifetime_seconds: 300,
        },
    );
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|_| "protected test API binds loopback".to_owned())?;
    let address = listener
        .local_addr()
        .map_err(|_| "protected test API local address is available".to_owned())?;
    let (stop, stopped) = oneshot::channel();
    let task = tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(async move {
                let _ = stopped.await;
            })
            .await
    });
    Ok(ApiHandle {
        url: format!("http://{address}"),
        stop,
        task,
    })
}

fn write_manifest(
    path: &Path,
    stream: &[u8],
    key: &str,
    record_count: u64,
) -> Result<String, String> {
    let manifest = serde_json::to_vec(&json!({
        "format":"litecr-migration/v1",
        "snapshot_id":format!("synthetic-snapshot-{key}"),
        "source_system_id":SOURCE,
        "target_registry_id":format!("disposition-reg-{key}"),
        "target_organization_id":format!("disposition-org-{key}"),
        "exported_at":"2026-10-12T00:00:00Z",
        "record_count":record_count,
        "stream_bytes":stream.len(),
        "stream_sha256":hex(&sha256(stream))
    }))
    .map_err(|_| "synthetic package manifest serializes".to_owned())?;
    fs::write(path, &manifest).map_err(|_| "synthetic package manifest is written".to_owned())?;
    Ok(hex(&sha256(&manifest)))
}

fn synthetic_line(key: &str, record_key: &str) -> Result<Vec<u8>, String> {
    let row = json!({
        "source_system_id":SOURCE,
        "source_record_key":record_key,
        "source_version":"v1",
        "public_id":format!("synthetic-public-{record_key}"),
        "assertions":[{"field_path":"name.original","original_value":format!("Synthetic {key}"),"language_tag":"en","script":"Latin"}],
        "identifiers":[],
        "relationships":[]
    });
    let mut bytes =
        serde_json::to_vec(&row).map_err(|_| "synthetic source row serializes".to_owned())?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn run_python(root: &Path, args: &[&str]) -> Result<Output, String> {
    let python = std::env::var_os("PYTHON").unwrap_or_else(|| {
        if cfg!(windows) {
            "python".into()
        } else {
            "python3".into()
        }
    });
    Command::new(python)
        .current_dir(root)
        .arg(root.join("tools/migration/litecr_migrate.py"))
        .args(args)
        .output()
        .map_err(|_| "synthetic Python migration stager starts (set PYTHON if needed)".to_owned())
}

async fn post_disposition(
    client: &reqwest::Client,
    url: &str,
    credential: &str,
    body: &Value,
) -> Result<(reqwest::StatusCode, Value), String> {
    let response = client
        .post(format!("{url}/api/v1/migrations/quarantine-dispositions"))
        .bearer_auth(credential)
        .json(body)
        .send()
        .await
        .map_err(|_| "protected synthetic disposition request completes".to_owned())?;
    let status = response.status();
    let bytes = response
        .bytes()
        .await
        .map_err(|_| "protected synthetic disposition response reads".to_owned())?;
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes)
            .map_err(|_| "protected disposition response is JSON".to_owned())?
    };
    Ok((status, value))
}

async fn run_cli(args: Vec<String>) -> Result<Output, String> {
    let binary = env!("CARGO_BIN_EXE_litecr").to_owned();
    tokio::task::spawn_blocking(move || Command::new(binary).args(args).output())
        .await
        .map_err(|_| "synthetic disposition CLI subprocess joins".to_owned())?
        .map_err(|_| "synthetic disposition CLI subprocess starts".to_owned())
}

async fn scenario(database_url: String, key: String) -> Result<(), String> {
    if !safe_synthetic_database(&database_url) {
        return Err("refusing a database without a test/synthetic/fixture name".into());
    }
    let pool = connect(&database_url, 8).await.map_err(|_| {
        "connects only to the configured disposable synthetic PostgreSQL".to_owned()
    })?;
    migrate(&pool)
        .await
        .map_err(|_| "synthetic test PostgreSQL migrations are current".to_owned())?;

    let subject = format!("disposition-operator-{key}");
    let read_subject = format!("disposition-reader-{key}");
    let wrong_subject = format!("disposition-wrong-scope-{key}");
    let registry = format!("disposition-reg-{key}");
    let organization = format!("disposition-org-{key}");
    let writer = provision(&pool, &subject, &key, "migration:write").await?;
    let reader = provision(&pool, &read_subject, &key, "migration:read").await?;
    let wrong_writer = provision(
        &pool,
        &wrong_subject,
        &format!("wrong-{key}"),
        "migration:write",
    )
    .await?;
    let api = start_api(pool.clone()).await?;

    let root = std::env::temp_dir().join(format!("litecr-disposition-e2e-{key}"));
    fs::create_dir(&root).map_err(|_| "unique synthetic temp directory is created".to_owned())?;
    make_private(&root, 0o700)
        .map_err(|_| "synthetic temp directory excludes group and other users".to_owned())?;
    fs::write(root.join(".owner"), "litecr-disposition-e2e-v1")
        .map_err(|_| "synthetic temp ownership marker is written".to_owned())?;
    let manifest = root.join("parent-manifest.json");
    let stream = root.join("parent.ndjson");
    let stage = root.join("parent-stage");
    let replacement_manifest = root.join("replacement-manifest.json");
    let replacement_stream = root.join("replacement.ndjson");
    let replacement_stage = root.join("replacement-stage");
    let request_file = root.join("disposition-request.json");
    let writer_credential_file = root.join("writer.credential");
    let reader_credential_file = root.join("reader.credential");
    fs::write(&writer_credential_file, &writer)
        .map_err(|_| "synthetic writer credential file is written".to_owned())?;
    fs::write(&reader_credential_file, &reader)
        .map_err(|_| "synthetic reader credential file is written".to_owned())?;
    let baseline_line = synthetic_line(&key, &format!("baseline-source-{key}"))?;
    let malformed_line = b"{malformed synthetic row}\n";
    let original = [baseline_line.as_slice(), malformed_line.as_slice()].concat();
    fs::write(&stream, &original)
        .map_err(|_| "malformed synthetic parent stream is written".to_owned())?;
    let parent_hash = write_manifest(&manifest, &original, &key, 2)?;
    let root_path = project_root();
    let parent_stage = run_python(
        &root_path,
        &[
            "stage",
            "--manifest",
            manifest.to_str().unwrap(),
            "--stream",
            stream.to_str().unwrap(),
            "--output",
            stage.to_str().unwrap(),
            "--batch-size",
            "1",
            "--batch-bytes",
            "8388608",
        ],
    )?;
    if parent_stage.status.code() != Some(2) {
        return Err(format!(
            "real Python stager quarantines malformed synthetic input (code {:?}): {}",
            parent_stage.status.code(),
            String::from_utf8_lossy(&parent_stage.stderr)
        ));
    }
    let quarantine: Value = serde_json::from_slice(
        &fs::read(stage.join("quarantine.json"))
            .map_err(|_| "Python stager writes quarantine evidence".to_owned())?,
    )
    .map_err(|_| "Python quarantine report is JSON".to_owned())?;
    if quarantine["exceptions"][0]["code"] != "ROW_INVALID_UTF8_OR_JSON"
        || quarantine["exceptions"].as_array().map(Vec::len) != Some(1)
        || quarantine["exceptions"][0]["line"] != 2
    {
        return Err(
            "Python stager reports the exact malformed synthetic line without source values".into(),
        );
    }

    let corrected_line = synthetic_line(&key, &format!("corrected-source-{key}"))?;
    let replacement_bytes = [baseline_line.as_slice(), corrected_line.as_slice()].concat();
    fs::write(&replacement_stream, &replacement_bytes)
        .map_err(|_| "synthetic replacement stream is written".to_owned())?;
    let replacement_hash = write_manifest(&replacement_manifest, &replacement_bytes, &key, 2)?;
    let replacement_result = run_python(
        &root_path,
        &[
            "stage",
            "--manifest",
            replacement_manifest.to_str().unwrap(),
            "--stream",
            replacement_stream.to_str().unwrap(),
            "--output",
            replacement_stage.to_str().unwrap(),
            "--batch-size",
            "1",
            "--batch-bytes",
            "8388608",
        ],
    )?;
    if !replacement_result.status.success() {
        return Err(format!(
            "real Python stager accepts corrected synthetic package: {}",
            String::from_utf8_lossy(&replacement_result.stderr)
        ));
    }
    let correction = json!({
        "input_line":2,
        "duplicate_of_line":null,
        "quarantine_code":"ROW_INVALID_UTF8_OR_JSON",
        "original_payload_sha256":quarantine["exceptions"][0]["payload_sha256"],
        "corrected_payload_sha256":hex(&sha256(&corrected_line))
    });
    let request = json!({
        "action":"corrected_resubmission",
        "parent_manifest_sha256":parent_hash,
        "replacement_manifest_sha256":replacement_hash,
        "registry_id":registry,
        "organization_id":organization,
        "reason":"synthetic reviewed format correction",
        "idempotency_key":format!("synthetic-disposition-{key}"),
        "corrections":[correction]
    });
    fs::write(
        &request_file,
        serde_json::to_vec(&request).map_err(|_| "hash-only request serializes".to_owned())?,
    )
    .map_err(|_| "hash-only disposition request file is written".to_owned())?;
    for private_file in [
        &writer_credential_file,
        &reader_credential_file,
        &manifest,
        &stream,
        &replacement_manifest,
        &replacement_stream,
        &request_file,
    ] {
        make_private(private_file, 0o600)
            .map_err(|_| "synthetic migration inputs exclude group and other users".to_owned())?;
    }
    let client = reqwest::Client::new();
    let before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM litecr.migration_quarantine_disposition WHERE registry_id=$1 AND organization_id=$2")
        .bind(&registry).bind(&organization).fetch_one(&pool).await.map_err(|_| "disposition baseline count reads".to_owned())?;
    let (denied_status, _) = post_disposition(&client, &api.url, &reader, &request).await?;
    if denied_status != reqwest::StatusCode::FORBIDDEN {
        return Err(format!(
            "migration:read credential disposition denied with HTTP 403 (got {denied_status})"
        ));
    }
    let after_denied: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM litecr.migration_quarantine_disposition WHERE registry_id=$1 AND organization_id=$2")
        .bind(&registry).bind(&organization).fetch_one(&pool).await.map_err(|_| "disposition count after denied request reads".to_owned())?;
    if after_denied != before {
        return Err("read-only denial leaves disposition table unchanged".into());
    }
    let (wrong_scope_status, _) =
        post_disposition(&client, &api.url, &wrong_writer, &request).await?;
    if !matches!(
        wrong_scope_status,
        reqwest::StatusCode::FORBIDDEN | reqwest::StatusCode::NOT_FOUND
    ) {
        return Err(format!(
            "migration:write credential outside the registry/organization scope is denied (got {wrong_scope_status})"
        ));
    }
    let after_wrong_scope: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM litecr.migration_quarantine_disposition WHERE registry_id=$1 AND organization_id=$2")
        .bind(&registry).bind(&organization).fetch_one(&pool).await.map_err(|_| "disposition count after wrong-scope request reads".to_owned())?;
    if after_wrong_scope != before {
        return Err("wrong-scope denial leaves disposition table unchanged".into());
    }

    let mut cli_args = vec![
        "migration".to_owned(),
        "disposition".to_owned(),
        "--request-file".to_owned(),
        request_file.to_string_lossy().into_owned(),
        "--manifest".to_owned(),
        manifest.to_string_lossy().into_owned(),
        "--stream".to_owned(),
        stream.to_string_lossy().into_owned(),
        "--stage-dir".to_owned(),
        stage.to_string_lossy().into_owned(),
        "--replacement-manifest".to_owned(),
        replacement_manifest.to_string_lossy().into_owned(),
        "--replacement-stream".to_owned(),
        replacement_stream.to_string_lossy().into_owned(),
        "--replacement-stage-dir".to_owned(),
        replacement_stage.to_string_lossy().into_owned(),
        "--credential-file".to_owned(),
        writer_credential_file.to_string_lossy().into_owned(),
        "--api-base-url".to_owned(),
        api.url.clone(),
    ];
    let accepted = run_cli(cli_args.clone()).await?;
    if !accepted.status.success()
        || !String::from_utf8_lossy(&accepted.stdout)
            .contains("quarantine disposition recorded: action corrected_resubmission")
        || String::from_utf8_lossy(&accepted.stdout).contains("(idempotent retry)")
    {
        return Err(format!(
            "real disposition CLI records reviewed synthetic package through the protected API: status={}, stdout={}, stderr={}",
            accepted.status,
            String::from_utf8_lossy(&accepted.stdout),
            String::from_utf8_lossy(&accepted.stderr)
        ));
    }
    let receipt_id: String = sqlx::query_scalar("SELECT disposition_id::text FROM litecr.migration_quarantine_disposition WHERE registry_id=$1 AND organization_id=$2")
        .bind(&registry).bind(&organization).fetch_one(&pool).await.map_err(|_| "disposition receipt is persisted".to_owned())?;
    let audited: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM litecr.migration_quarantine_disposition d JOIN litecr.migration_quarantine_disposition_item i USING(disposition_id) JOIN litecr.mutation_audit a ON a.resource_ref=d.disposition_id::text AND a.action='migration.quarantine.disposition' JOIN litecr.outbox_event o ON o.aggregate_id=d.disposition_id AND o.event_type='migration.quarantine.disposition' WHERE d.disposition_id=$1::uuid AND i.input_line=2 AND i.quarantine_code='ROW_INVALID_UTF8_OR_JSON' AND a.actor_id=$2 AND d.registry_id=$3 AND d.organization_id=$4")
        .bind(&receipt_id).bind(&subject).bind(&registry).bind(&organization).fetch_one(&pool).await.map_err(|_| "disposition audit/outbox reconciliation query runs".to_owned())?;
    if audited != 1 {
        return Err(
            "disposition, evidence, actor audit and outbox are atomically represented".into(),
        );
    }

    let replay = run_cli(std::mem::take(&mut cli_args)).await?;
    if !replay.status.success()
        || !String::from_utf8_lossy(&replay.stdout)
            .contains("quarantine disposition recorded: action corrected_resubmission")
        || !String::from_utf8_lossy(&replay.stdout).contains("(idempotent retry)")
    {
        return Err(format!(
            "real disposition CLI exact retry returns the durable receipt: stdout={}, stderr={}",
            String::from_utf8_lossy(&replay.stdout),
            String::from_utf8_lossy(&replay.stderr)
        ));
    }
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM litecr.migration_quarantine_disposition WHERE registry_id=$1 AND organization_id=$2")
        .bind(&registry).bind(&organization).fetch_one(&pool).await.map_err(|_| "retry disposition count reads".to_owned())?;
    if count != before + 1 {
        return Err("exact retry creates no duplicate disposition".into());
    }

    api.stop().await?;
    pool.close().await;
    remove_owned_temp_fixture(&root);
    Ok(())
}

async fn cleanup(url: &str, key: &str) {
    let Ok(pool) = connect(url, 4).await else {
        return;
    };
    let subject = format!("disposition-operator-{key}");
    let reader = format!("disposition-reader-{key}");
    let wrong = format!("disposition-wrong-scope-{key}");
    let registry = format!("disposition-reg-{key}");
    let organization = format!("disposition-org-{key}");
    let subjects = [subject, reader, wrong];
    let _ = sqlx::query("DELETE FROM litecr.migration_quarantine_disposition_item WHERE disposition_id IN (SELECT disposition_id FROM litecr.migration_quarantine_disposition WHERE registry_id=$1 AND organization_id=$2)")
        .bind(&registry).bind(&organization).execute(&pool).await;
    let _ = sqlx::query("DELETE FROM litecr.migration_quarantine_disposition WHERE registry_id=$1 AND organization_id=$2")
        .bind(&registry).bind(&organization).execute(&pool).await;
    let _ = sqlx::query(
        "DELETE FROM litecr.mutation_audit WHERE registry_id=$1 AND organization_id=$2",
    )
    .bind(&registry)
    .bind(&organization)
    .execute(&pool)
    .await;
    let _ =
        sqlx::query("DELETE FROM litecr.outbox_event WHERE registry_id=$1 AND organization_id=$2")
            .bind(&registry)
            .bind(&organization)
            .execute(&pool)
            .await;
    let _ = sqlx::query("DELETE FROM litecr.access_integration_credential WHERE issuer_url='' AND external_subject=ANY($1::text[])")
        .bind(&subjects).execute(&pool).await;
    let _ = sqlx::query("DELETE FROM litecr.access_audit WHERE actor_kind='integration_client' AND actor_issuer_url='' AND actor_id=ANY($1::text[])")
        .bind(&subjects).execute(&pool).await;
    let _ = sqlx::query("DELETE FROM litecr.access_grant WHERE actor_kind='integration_client' AND issuer_url='' AND external_subject=ANY($1::text[])")
        .bind(&subjects).execute(&pool).await;
    let _ = sqlx::query("DELETE FROM litecr.access_identity WHERE actor_kind='integration_client' AND issuer_url='' AND external_subject=ANY($1::text[])")
        .bind(&subjects).execute(&pool).await;
    pool.close().await;
    let expected_root = std::env::temp_dir().join(format!("litecr-disposition-e2e-{key}"));
    remove_owned_temp_fixture(&expected_root);
    if let Ok(entries) = fs::read_dir(std::env::temp_dir()) {
        for entry in entries.flatten() {
            let candidate = entry.path();
            let is_our_fixture = candidate
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("litecr-disposition-e2e-"))
                && fs::read_to_string(candidate.join(".owner"))
                    .is_ok_and(|marker| marker == "litecr-disposition-e2e-v1");
            if is_our_fixture {
                remove_owned_temp_fixture(&candidate);
            }
        }
    }
}

#[tokio::test]
async fn python_stage_and_real_cli_disposition_preserve_scope_audit_and_idempotency() {
    let url = std::env::var(DB_ENV).expect(
        "set LITECR_TEST_DATABASE_URL to the serialized disposable synthetic PostgreSQL URL",
    );
    assert!(
        safe_synthetic_database(&url),
        "test database name must explicitly identify synthetic/test/fixture data"
    );
    let key = format!(
        "{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock is after epoch")
            .as_nanos()
    );
    let result = tokio::spawn(scenario(url.clone(), key.clone())).await;
    cleanup(&url, &key).await;
    assert!(
        result.is_ok(),
        "synthetic disposition task joins: {result:?}"
    );
    result
        .unwrap()
        .expect("Python staging and protected CLI/PostgreSQL disposition scenario");
}
