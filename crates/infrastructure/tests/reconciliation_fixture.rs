//! End-to-end reconciliation of the declared synthetic migration fixture.
//!
//! This deliberately exercises the supported staged subset through the same
//! MigrationService/PostgresMigrationExecutor path used by the operator API.
//! The fixture's duplicate replay and historical relationship remain in the
//! staged exception files and are never imported.

use async_trait::async_trait;
use litecr_application::{
    access::{
        AccessService, AccessServiceConfig, AccessSessionRepository, ActorKind, ActorRef,
        OidcAuthorization, OidcFlow, OidcIdentityClaims, OidcProvider, OidcProviderError,
    },
    migration::{MigrationBatch, MigrationRecord, MigrationRun, MigrationService, ReverseManifest},
};
use litecr_infrastructure::{
    access::{
        PostgresAccessAuthenticator, PostgresAccessDirectory, PostgresAccessSessionRepository,
        PostgresOidcFlowStore,
    },
    connect, migrate,
    migration::{MigrationRowRefusal, PostgresMigrationExecutor, map_supported_record},
};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

const ISSUER: &str = "https://litecr-reconciliation-test.invalid";
const SOURCE: &str = "synthetic-opencr-export";
const REGISTRY: &str = "synthetic-registry";
const ORGANIZATION: &str = "synthetic-organization";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Counts {
    persons: i64,
    active_sources: i64,
    withdrawn_sources: i64,
    active_versions: i64,
    withdrawn_versions: i64,
    active_assertions: i64,
    withdrawn_assertions: i64,
    active_identifier_assertions: i64,
    withdrawn_identifier_assertions: i64,
    aliases: i64,
    identifiers: i64,
    checkpoints: i64,
    batches: i64,
    journal_rows: i64,
    reverse_receipts: i64,
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("infrastructure crate is inside workspace")
        .to_path_buf()
}

fn synthetic_database_name(database_url: &str) -> &str {
    database_url
        .split('?')
        .next()
        .and_then(|url| url.rsplit('/').next())
        .unwrap_or_default()
}

fn require_synthetic_database(database_url: &str) {
    let name = synthetic_database_name(database_url).to_ascii_lowercase();
    assert_ne!(
        name, "litecr_access_test",
        "refusing the access test database"
    );
    assert!(
        ["test", "synthetic", "fixture"]
            .iter()
            .any(|marker| name.contains(marker)),
        "database name must explicitly identify test/synthetic/fixture data"
    );
}

fn unique_key() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_nanos();
    format!("{}-{nanos}", std::process::id())
}

fn python_executable() -> std::ffi::OsString {
    std::env::var_os("PYTHON").unwrap_or_else(|| {
        if cfg!(windows) {
            "python".into()
        } else {
            "python3".into()
        }
    })
}

fn stage_fixture(root: &Path, stage_dir: &Path) -> (MigrationRun, MigrationBatch, Value, Value) {
    let manifest_path = root.join("tests/reconciliation/fixtures/v1-manifest.json");
    let stream_path = root.join("tests/reconciliation/fixtures/v1-records.ndjson");
    let tool_path = root.join("tools/migration/litecr_migrate.py");
    let output = Command::new(python_executable())
        .current_dir(root)
        .arg(&tool_path)
        .arg("stage")
        .arg("--manifest")
        .arg(&manifest_path)
        .arg("--stream")
        .arg(&stream_path)
        .arg("--output")
        .arg(stage_dir)
        .output()
        .expect("Python must be available in the test environment; set PYTHON to its executable");
    assert!(
        output.status.success() || output.status.code() == Some(2),
        "declared fixture staging failed outside the expected quarantine status: code={:?}, stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );

    let manifest_bytes = fs::read(&manifest_path).expect("declared manifest exists");
    let manifest: Value = serde_json::from_slice(&manifest_bytes).expect("manifest is JSON");
    assert_eq!(manifest["source_system_id"], SOURCE);
    let manifest_digest = sha256(&manifest_bytes);
    let stream_bytes = fs::read(&stream_path).expect("declared fixture stream exists");
    let stream_digest = sha256(&stream_bytes);
    let checkpoint: Value = serde_json::from_slice(
        &fs::read(stage_dir.join("checkpoint.json")).expect("stage checkpoint exists"),
    )
    .expect("stage checkpoint is JSON");
    let quarantine: Value = serde_json::from_slice(
        &fs::read(stage_dir.join("quarantine.json")).expect("quarantine report exists"),
    )
    .expect("quarantine report is JSON");
    let replays: Value = serde_json::from_slice(
        &fs::read(stage_dir.join("replays.json")).expect("replay report exists"),
    )
    .expect("replay report is JSON");
    assert_eq!(checkpoint["manifest_sha256"], hex(&manifest_digest));
    assert_eq!(checkpoint["stream_sha256"], hex(&stream_digest));
    assert_eq!(checkpoint["record_count"], manifest["record_count"]);
    assert_eq!(checkpoint["valid_records"], 4);
    assert_eq!(checkpoint["exact_replay_duplicates"], 1);
    assert_eq!(checkpoint["quarantined_records"], 1);
    let stage_summary: Value = serde_json::from_slice(&output.stdout)
        .expect("stager emits a machine-readable report even with quarantined rows");
    assert_eq!(stage_summary["quarantined_records"], 1);
    assert_eq!(
        output.status.success(),
        stage_summary["quarantined_records"] == 0,
        "stager exit status agrees with whether its package contains exceptions"
    );
    assert_eq!(quarantine["exceptions"].as_array().unwrap().len(), 1);
    assert_eq!(
        quarantine["exceptions"][0]["code"],
        "HISTORICAL_RELATIONSHIPS_UNSUPPORTED"
    );
    assert_eq!(replays["replays"].as_array().unwrap().len(), 1);

    let batch_meta: Value = serde_json::from_slice(
        &fs::read(stage_dir.join("batches/00000001.json")).expect("staged batch metadata exists"),
    )
    .expect("staged batch metadata is JSON");
    let exact_bytes =
        fs::read(stage_dir.join("batches/00000001.ndjson")).expect("supported subset batch exists");
    assert_eq!(batch_meta["record_count"], 4);
    assert_eq!(batch_meta["manifest_sha256"], hex(&manifest_digest));
    assert_eq!(batch_meta["stream_sha256"], hex(&stream_digest));
    assert_eq!(batch_meta["batch_sha256"], hex(&sha256(&exact_bytes)));
    let raw_lines = exact_bytes
        .split_inclusive(|byte| *byte == b'\n')
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    assert_eq!(raw_lines.len(), 4);
    let staged_records = raw_lines
        .into_iter()
        .map(|raw_ndjson| {
            let row: Value =
                serde_json::from_slice(raw_ndjson.strip_suffix(b"\n").expect("staged row has LF"))
                    .expect("staged row is JSON");
            let canonical = serde_json::to_vec(&row).expect("row serializes canonically");
            MigrationRecord {
                source_system_id: row["source_system_id"].as_str().unwrap().to_owned(),
                source_record_key: row["source_record_key"].as_str().unwrap().to_owned(),
                source_version: row["source_version"].as_str().unwrap().to_owned(),
                payload_sha256: sha256(&canonical),
                raw_ndjson,
            }
        })
        .collect::<Vec<_>>();
    let run = MigrationRun {
        manifest_sha256: manifest_digest,
        stream_sha256: stream_digest,
        snapshot_id: manifest["snapshot_id"].as_str().unwrap().to_owned(),
        source_system_id: manifest["source_system_id"].as_str().unwrap().to_owned(),
        registry_id: manifest["target_registry_id"].as_str().unwrap().to_owned(),
        organization_id: manifest["target_organization_id"]
            .as_str()
            .unwrap()
            .to_owned(),
        // The importer is intentionally limited to four supported staged rows;
        // the duplicate and unsupported relationship remain offline exceptions.
        expected_record_count: 4,
    };
    let mut supported_records = Vec::new();
    let mut refused_keys = Vec::new();
    for record in staged_records {
        match map_supported_record(&run, &record) {
            Ok(_) => supported_records.push(record),
            Err(reason) => {
                refused_keys.push((record.source_record_key, reason));
            }
        }
    }
    assert_eq!(
        refused_keys,
        vec![("src-001".to_owned(), MigrationRowRefusal::InvalidAliases)],
        "the actual Registry mapper fail-closes on duplicate CRUID/public ID instead of silently rewriting it"
    );
    assert_eq!(supported_records.len(), 3);
    let mut supported_run = run;
    supported_run.expected_record_count = supported_records.len() as u64;
    let exact_bytes = supported_records
        .iter()
        .flat_map(|record| record.raw_ndjson.iter().copied())
        .collect::<Vec<_>>();
    let source_line_by_key = fs::read(&stream_path)
        .expect("source stream exists")
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .enumerate()
        .map(|(index, line)| {
            let row: Value = serde_json::from_slice(line).expect("source line is JSON");
            (
                row["source_record_key"].as_str().unwrap().to_owned(),
                index as u64 + 1,
            )
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    let lines = supported_records
        .iter()
        .map(|record| source_line_by_key[&record.source_record_key])
        .collect::<Vec<_>>();
    let batch = MigrationBatch {
        run: supported_run.clone(),
        sequence: batch_meta["sequence"].as_u64().unwrap(),
        first_input_line: *lines.iter().min().unwrap(),
        last_input_line: *lines.iter().max().unwrap(),
        batch_sha256: sha256(&exact_bytes),
        exact_bytes,
        records: supported_records,
    };
    (supported_run, batch, quarantine, replays)
}

async fn scoped_counts(pool: &PgPool, manifest_sha: [u8; 32]) -> Counts {
    let row = sqlx::query(
        "SELECT \
           (SELECT COUNT(*) FROM litecr.person WHERE registry_id=$1 AND organization_id=$2) AS persons, \
           (SELECT COUNT(*) FROM litecr.source_record WHERE registry_id=$1 AND organization_id=$2 AND withdrawn_at IS NULL) AS active_sources, \
           (SELECT COUNT(*) FROM litecr.source_record WHERE registry_id=$1 AND organization_id=$2 AND withdrawn_at IS NOT NULL) AS withdrawn_sources, \
           (SELECT COUNT(*) FROM litecr.source_record_version v JOIN litecr.source_record s USING(source_record_id) WHERE s.registry_id=$1 AND s.organization_id=$2 AND v.withdrawn_at IS NULL) AS active_versions, \
           (SELECT COUNT(*) FROM litecr.source_record_version v JOIN litecr.source_record s USING(source_record_id) WHERE s.registry_id=$1 AND s.organization_id=$2 AND v.withdrawn_at IS NOT NULL) AS withdrawn_versions, \
           (SELECT COUNT(*) FROM litecr.source_assertion a JOIN litecr.source_record s USING(source_record_id) WHERE s.registry_id=$1 AND s.organization_id=$2 AND a.withdrawn_at IS NULL) AS active_assertions, \
           (SELECT COUNT(*) FROM litecr.source_assertion a JOIN litecr.source_record s USING(source_record_id) WHERE s.registry_id=$1 AND s.organization_id=$2 AND a.withdrawn_at IS NOT NULL) AS withdrawn_assertions, \
           (SELECT COUNT(*) FROM litecr.source_identifier_assertion a JOIN litecr.source_record s USING(source_record_id) WHERE s.registry_id=$1 AND s.organization_id=$2 AND a.withdrawn_at IS NULL) AS active_identifier_assertions, \
           (SELECT COUNT(*) FROM litecr.source_identifier_assertion a JOIN litecr.source_record s USING(source_record_id) WHERE s.registry_id=$1 AND s.organization_id=$2 AND a.withdrawn_at IS NOT NULL) AS withdrawn_identifier_assertions, \
           (SELECT COUNT(*) FROM litecr.person_alias WHERE registry_id=$1 AND organization_id=$2) AS aliases, \
           (SELECT COUNT(*) FROM litecr.person_identifier WHERE registry_id=$1 AND organization_id=$2) AS identifiers, \
           (SELECT COUNT(*) FROM litecr.migration_checkpoint WHERE manifest_sha256=$3) AS checkpoints, \
           (SELECT COUNT(*) FROM litecr.migration_batch WHERE manifest_sha256=$3) AS batches, \
           (SELECT COUNT(*) FROM litecr.migration_journal WHERE manifest_sha256=$3) AS journal_rows, \
           (SELECT COUNT(*) FROM litecr.migration_reverse_receipt WHERE manifest_sha256=$3) AS reverse_receipts",
    )
    .bind(REGISTRY)
    .bind(ORGANIZATION)
    .bind(manifest_sha.to_vec())
    .fetch_one(pool)
    .await
    .expect("independent fixture reconciliation query succeeds");
    Counts {
        persons: row.get("persons"),
        active_sources: row.get("active_sources"),
        withdrawn_sources: row.get("withdrawn_sources"),
        active_versions: row.get("active_versions"),
        withdrawn_versions: row.get("withdrawn_versions"),
        active_assertions: row.get("active_assertions"),
        withdrawn_assertions: row.get("withdrawn_assertions"),
        active_identifier_assertions: row.get("active_identifier_assertions"),
        withdrawn_identifier_assertions: row.get("withdrawn_identifier_assertions"),
        aliases: row.get("aliases"),
        identifiers: row.get("identifiers"),
        checkpoints: row.get("checkpoints"),
        batches: row.get("batches"),
        journal_rows: row.get("journal_rows"),
        reverse_receipts: row.get("reverse_receipts"),
    }
}

async fn provisioned_access(
    pool: &PgPool,
    suffix: &str,
) -> litecr_application::access::AccessContext {
    let subject = format!("synthetic-fixture-registrar-{suffix}");
    sqlx::query("INSERT INTO litecr.access_identity (actor_kind,issuer_url,external_subject) VALUES ('human',$1,$2)")
        .bind(ISSUER).bind(&subject).execute(pool).await.expect("synthetic actor identity inserted");
    sqlx::query("INSERT INTO litecr.access_grant (actor_kind,issuer_url,external_subject,registry_id,organization_id,record_ids,role_name,permission_name) VALUES ('human',$1,$2,$3,$4,NULL,'registrar',NULL)")
        .bind(ISSUER).bind(&subject).bind(REGISTRY).bind(ORGANIZATION).execute(pool).await.expect("synthetic scoped registrar grant inserted");
    sqlx::query("INSERT INTO litecr.access_grant (actor_kind,issuer_url,external_subject,registry_id,organization_id,record_ids,role_name,permission_name) VALUES ('human',$1,$2,$3,$4,NULL,NULL,'migration:write')")
        .bind(ISSUER).bind(&subject).bind(REGISTRY).bind(ORGANIZATION).execute(pool).await.expect("synthetic scoped migration permission inserted");
    let directory = PostgresAccessDirectory::new(pool.clone());
    let sessions = PostgresAccessSessionRepository::new(pool.clone(), directory.clone());
    let actor = ActorRef {
        kind: ActorKind::Human,
        id: subject,
        issuer_url: Some(ISSUER.to_owned()),
    };
    let session = sessions
        .create(
            &actor,
            Some("synthetic-fixture-session"),
            600,
            "fixture-session-create",
        )
        .await
        .expect("synthetic registrar session created");
    let service = AccessService::new(
        Arc::new(UnusedOidc),
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
    .expect("synthetic access service configuration valid");
    service
        .authenticate(
            &session.opaque_token,
            Some(&session.csrf_token),
            true,
            "fixture-authenticate",
        )
        .await
        .expect("existing authenticator produces authorized synthetic context")
}

struct UnusedOidc;

#[async_trait]
impl OidcProvider for UnusedOidc {
    async fn begin_authorization(&self) -> Result<OidcAuthorization, OidcProviderError> {
        Err(OidcProviderError::Unavailable)
    }
    async fn exchange_code(
        &self,
        _code: &str,
        _flow: &OidcFlow,
    ) -> Result<OidcIdentityClaims, OidcProviderError> {
        Err(OidcProviderError::Unavailable)
    }
}

#[tokio::test]
async fn declared_fixture_supported_subset_reconciles_forward_and_source_only_reverse() {
    let database_url = std::env::var("LITECR_TEST_DATABASE_URL")
        .expect("set LITECR_TEST_DATABASE_URL to dedicated synthetic litecr_test database");
    require_synthetic_database(&database_url);
    let suffix = unique_key();
    // Recover only this declared fixture's fixed synthetic scope if an earlier
    // interrupted test process stopped during teardown.
    cleanup(&database_url, &suffix).await;
    let outcome = tokio::spawn(run_fixture(database_url.clone(), suffix.clone())).await;
    cleanup(&database_url, &suffix).await;
    assert!(outcome.is_ok(), "fixture test task panicked: {outcome:?}");
    outcome
        .unwrap()
        .expect("fixture reconciliation scenario passes");
}

async fn run_fixture(database_url: String, suffix: String) -> Result<(), String> {
    let pool = connect(&database_url, 8)
        .await
        .map_err(|_| "synthetic PostgreSQL connection failed")?;
    let connected_database: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(&pool)
        .await
        .map_err(|_| "connected database name inspected before writes")?;
    ensure(
        connected_database == "litecr_test" && connected_database != "litecr_access_test",
        "test refuses any database other than the dedicated synthetic litecr_test database",
    )?;
    migrate(&pool)
        .await
        .map_err(|_| "synthetic PostgreSQL schema setup failed")?;
    let root = fixture_root();
    let staging_dir = std::env::temp_dir().join(format!("litecr-reconciliation-{suffix}"));
    fs::create_dir_all(&staging_dir).map_err(|_| "fixture stage directory created")?;
    let (run, batch, quarantine, replays) = stage_fixture(&root, &staging_dir);

    let before = scoped_counts(&pool, run.manifest_sha256).await;
    ensure(
        before == Counts::default(),
        "fixture target scope must be empty before import",
    )?;
    let context = provisioned_access(&pool, &suffix).await;
    let service = MigrationService::new(Box::new(PostgresMigrationExecutor::new(pool.clone())));
    service
        .validate_batch(Some(&context), &batch)
        .await
        .map_err(|error| {
            format!("staged supported subset no-write import validation failed: {error:?}")
        })?;
    let after_validate = scoped_counts(&pool, run.manifest_sha256).await;
    ensure(
        after_validate == before,
        "no-write validation leaves Registry and journal counts unchanged",
    )?;

    let commit = service
        .commit_batch(Some(&context), batch)
        .await
        .map_err(|error| format!("staged supported subset import failed: {error:?}"))?;
    ensure(
        commit.checkpoint.committed_record_count == 3 && commit.applied.len() == 3,
        "forward receipt reconciles the three Registry-mappable staged rows",
    )?;
    let after_import = scoped_counts(&pool, run.manifest_sha256).await;
    ensure(
        after_import
            == Counts {
                persons: 3,
                active_sources: 3,
                withdrawn_sources: 0,
                active_versions: 3,
                withdrawn_versions: 0,
                active_assertions: 6,
                withdrawn_assertions: 0,
                active_identifier_assertions: 2,
                withdrawn_identifier_assertions: 0,
                aliases: 4,
                identifiers: 1,
                checkpoints: 1,
                batches: 1,
                journal_rows: 3,
                reverse_receipts: 0,
            },
        "Registry facts and durable manifest counts independently match the mapper-supported staged subset",
    )?;
    verify_representative_source_facts(&pool).await?;

    let reverse = service
        .reverse_manifest(
            Some(&context),
            ReverseManifest {
                manifest_sha256: run.manifest_sha256,
                registry_id: run.registry_id.clone(),
                organization_id: run.organization_id.clone(),
                reason: "synthetic fixture source-only withdrawal reconciliation".to_owned(),
            },
        )
        .await
        .map_err(|_| "fixture manifest source-only reverse succeeds")?;
    ensure(
        reverse.withdrawn_record_count == 3
            && reverse.detached_identifier_count == 1
            && reverse.retained_alias_count == 4,
        "reverse receipt accounts for three withdrawn source rows, one identifier and retained aliases",
    )?;
    let after_reverse = scoped_counts(&pool, run.manifest_sha256).await;
    ensure(
        after_reverse
            == Counts {
                persons: 3,
                active_sources: 0,
                withdrawn_sources: 3,
                active_versions: 0,
                withdrawn_versions: 3,
                active_assertions: 0,
                withdrawn_assertions: 6,
                active_identifier_assertions: 0,
                withdrawn_identifier_assertions: 2,
                aliases: 4,
                identifiers: 0,
                checkpoints: 1,
                batches: 1,
                journal_rows: 3,
                reverse_receipts: 1,
            },
        "reverse withdraws only mapper-supported source evidence and retains identities, aliases and journal",
    )?;
    let checkpoint_state: String = sqlx::query_scalar(
        "SELECT state FROM litecr.migration_checkpoint WHERE manifest_sha256=$1",
    )
    .bind(run.manifest_sha256.to_vec())
    .fetch_one(&pool)
    .await
    .map_err(|_| "reversed checkpoint state is independently queryable")?;
    ensure(
        checkpoint_state == "reversed",
        "manifest checkpoint is durably reversed",
    )?;

    // The independent staged reports remain the same after target reversal:
    // no quarantined relationship or exact replay was silently accepted.
    ensure(
        quarantine["exceptions"]
            .as_array()
            .is_some_and(|items| items.len() == 1)
            && quarantine["exceptions"][0]["code"] == "HISTORICAL_RELATIONSHIPS_UNSUPPORTED"
            && replays["replays"]
                .as_array()
                .is_some_and(|items| items.len() == 1),
        "package exception and duplicate counts remain explicit and outside the import",
    )?;
    fs::remove_dir_all(&staging_dir).map_err(|_| "synthetic stage directory removed")?;
    pool.close().await;
    Ok(())
}

async fn verify_representative_source_facts(pool: &PgPool) -> Result<(), String> {
    let facts = sqlx::query(
        "SELECT s.source_record_key, p.public_id, sv.source_version, a.field_path, a.original_value, a.language_tag, a.script, s.source_record_id::text AS source_record_id \
         FROM litecr.source_record s JOIN litecr.person p USING(person_id) \
         JOIN litecr.source_record_version sv ON sv.source_record_id=s.source_record_id AND sv.receive_version=s.current_receive_version \
         JOIN litecr.source_assertion a ON a.source_record_id=s.source_record_id AND a.receive_version=sv.receive_version \
         WHERE s.registry_id=$1 AND s.organization_id=$2 ORDER BY s.source_record_key,a.field_path",
    )
    .bind(REGISTRY)
    .bind(ORGANIZATION)
    .fetch_all(pool)
    .await
    .map_err(|_| "representative source facts queried independently")?;
    ensure(
        facts.len() == 6,
        "all six source assertions are represented",
    )?;
    let partial_birth: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM litecr.migration_journal j WHERE j.source_record_key='src-002' AND convert_from(j.original_payload_ndjson,'UTF8') LIKE '%1984%' AND convert_from(j.original_payload_ndjson,'UTF8') LIKE '%year%')",
    )
    .fetch_one(pool)
    .await
    .map_err(|_| "original partial-date precision evidence queried from migration journal")?;
    ensure(
        partial_birth,
        "partial-year original value and precision remain in exact journal source bytes",
    )?;
    let lao_name: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM litecr.source_assertion a JOIN litecr.source_record s USING(source_record_id) WHERE s.registry_id=$1 AND s.organization_id=$2 AND s.source_record_key='src-003' AND a.field_path='name.original' AND a.original_value='Nguyễn Thị Ví dụ' AND a.language_tag='vi' AND a.script='Latin')",
    )
    .bind(REGISTRY)
    .bind(ORGANIZATION)
    .fetch_one(pool)
    .await
    .map_err(|_| "original-script source assertion queried independently")?;
    ensure(
        lao_name,
        "source assertion preserves original Vietnamese text and script metadata",
    )?;
    let identifiers: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM litecr.source_identifier_assertion a JOIN litecr.source_record s USING(source_record_id) WHERE s.registry_id=$1 AND s.organization_id=$2 AND a.assertion_state='accepted'",
    )
    .bind(REGISTRY)
    .bind(ORGANIZATION)
    .fetch_one(pool)
    .await
    .map_err(|_| "source identifier assertions queried independently")?;
    ensure(
        identifiers == 2,
        "source identifier assertions reconcile independently",
    )?;
    Ok(())
}

async fn cleanup(database_url: &str, suffix: &str) {
    let pool = connect(database_url, 4)
        .await
        .expect("synthetic test database available for scoped cleanup");
    let source_ids: Vec<String> = sqlx::query_scalar(
        "SELECT source_record_id::text FROM litecr.source_record WHERE registry_id=$1 AND organization_id=$2 AND source_system_id=$3 AND source_record_key=ANY($4::text[])",
    )
    .bind(REGISTRY)
    .bind(ORGANIZATION)
    .bind(SOURCE)
    .bind(["src-002", "src-003", "src-004"])
    .fetch_all(&pool)
    .await
    .expect("fixture-owned source ids selected by exact scope");
    let manifest_hash: Vec<u8> = Sha256::digest(
        fs::read(fixture_root().join("tests/reconciliation/fixtures/v1-manifest.json"))
            .expect("declared manifest exists for cleanup key"),
    )
    .to_vec();
    sqlx::query("DELETE FROM litecr.migration_reverse_receipt WHERE manifest_sha256=$1")
        .bind(&manifest_hash)
        .execute(&pool)
        .await
        .expect("fixture reverse receipt removed before journal");
    sqlx::query("DELETE FROM litecr.migration_effect WHERE manifest_sha256=$1")
        .bind(&manifest_hash)
        .execute(&pool)
        .await
        .expect("fixture effect rows removed before identity cleanup");
    sqlx::query("DELETE FROM litecr.migration_journal WHERE manifest_sha256=$1")
        .bind(&manifest_hash)
        .execute(&pool)
        .await
        .expect("fixture journal rows removed");
    sqlx::query("DELETE FROM litecr.migration_batch WHERE manifest_sha256=$1")
        .bind(&manifest_hash)
        .execute(&pool)
        .await
        .expect("fixture batch removed");
    sqlx::query("DELETE FROM litecr.migration_checkpoint WHERE manifest_sha256=$1")
        .bind(&manifest_hash)
        .execute(&pool)
        .await
        .expect("fixture checkpoint removed");
    if !source_ids.is_empty() {
        sqlx::query("DELETE FROM litecr.registry_idempotency WHERE registry_id=$1 AND source_record_id::text=ANY($2::text[])")
            .bind(REGISTRY).bind(&source_ids).execute(&pool).await
            .expect("fixture source idempotency rows removed by exact scope and source ids");
        sqlx::query("DELETE FROM litecr.consumer_receipt WHERE consumer_name='advisory-matcher-v1' AND event_id IN (SELECT event_id FROM litecr.consumer_job WHERE registry_id=$1 AND organization_id=$2 AND source_record_id::text=ANY($3::text[]))")
            .bind(REGISTRY).bind(ORGANIZATION).bind(&source_ids).execute(&pool).await
            .expect("fixture consumer receipts removed before their jobs");
        sqlx::query("DELETE FROM litecr.consumer_job WHERE registry_id=$1 AND organization_id=$2 AND source_record_id::text=ANY($3::text[])")
            .bind(REGISTRY).bind(ORGANIZATION).bind(&source_ids).execute(&pool).await
            .expect("fixture queue jobs removed before referenced source rows");
        sqlx::query("DELETE FROM litecr.person_identifier WHERE registry_id=$1 AND organization_id=$2 AND source_record_id::text=ANY($3::text[])")
            .bind(REGISTRY).bind(ORGANIZATION).bind(&source_ids).execute(&pool).await
            .expect("fixture person identifiers removed before referenced source rows");
        sqlx::query("DELETE FROM matching.normalized_assertion WHERE source_record_id::text=ANY($1::text[])")
            .bind(&source_ids).execute(&pool).await
            .expect("fixture normalized assertion keys removed before source assertions");
        sqlx::query("DELETE FROM matching.normalized_identifier WHERE source_record_id::text=ANY($1::text[])")
            .bind(&source_ids).execute(&pool).await
            .expect("fixture normalized identifier keys removed before source versions");
        sqlx::query("DELETE FROM matching.indexed_source_version WHERE source_record_id::text=ANY($1::text[])")
            .bind(&source_ids).execute(&pool).await
            .expect("fixture matching index markers removed before source versions");
        for table in [
            "source_identifier_assertion",
            "source_assertion",
            "source_record_version",
        ] {
            let sql =
                format!("DELETE FROM litecr.{table} WHERE source_record_id::text=ANY($1::text[])");
            sqlx::query(&sql)
                .bind(&source_ids)
                .execute(&pool)
                .await
                .expect("fixture source evidence removed");
        }
        sqlx::query(
            "DELETE FROM litecr.source_record WHERE source_record_id::text=ANY($1::text[])",
        )
        .bind(&source_ids)
        .execute(&pool)
        .await
        .expect("fixture source rows removed");
    }
    let person_ids: Vec<String> = sqlx::query_scalar(
        "SELECT person_id::text FROM litecr.person WHERE registry_id=$1 AND organization_id=$2 AND public_id=ANY($3::text[])",
    )
    .bind(REGISTRY)
    .bind(ORGANIZATION)
    .bind(["CRUID-SYN-002", "CRUID-SYN-003", "CRUID-SYN-004"])
    .fetch_all(&pool)
    .await
    .expect("fixture persons selected by exact scope");
    if !person_ids.is_empty() {
        sqlx::query("DELETE FROM litecr.person_alias WHERE person_id::text=ANY($1::text[])")
            .bind(&person_ids)
            .execute(&pool)
            .await
            .expect("fixture aliases removed by exact imported person ids");
        sqlx::query("DELETE FROM litecr.person WHERE person_id::text=ANY($1::text[])")
            .bind(&person_ids)
            .execute(&pool)
            .await
            .expect("fixture persons removed");
    }
    let fixture_subjects: Vec<String> = sqlx::query_scalar(
        "SELECT external_subject FROM litecr.access_grant WHERE actor_kind='human' AND issuer_url=$1 AND registry_id=$2 AND organization_id=$3 AND external_subject LIKE 'synthetic-fixture-registrar-%'",
    )
    .bind(ISSUER)
    .bind(REGISTRY)
    .bind(ORGANIZATION)
    .fetch_all(&pool)
    .await
    .expect("fixture access subjects selected by exact synthetic grant scope");
    for fixture_subject in fixture_subjects {
        sqlx::query("DELETE FROM litecr.access_session WHERE actor_kind='human' AND issuer_url=$1 AND external_subject=$2")
            .bind(ISSUER).bind(&fixture_subject).execute(&pool).await.expect("fixture session removed");
        sqlx::query("DELETE FROM litecr.access_grant WHERE actor_kind='human' AND issuer_url=$1 AND external_subject=$2")
            .bind(ISSUER).bind(&fixture_subject).execute(&pool).await.expect("fixture grant removed");
        sqlx::query("DELETE FROM litecr.access_audit WHERE actor_issuer_url=$1 AND actor_id=$2")
            .bind(ISSUER)
            .bind(&fixture_subject)
            .execute(&pool)
            .await
            .expect("fixture access audit removed");
        sqlx::query("DELETE FROM litecr.access_identity WHERE actor_kind='human' AND issuer_url=$1 AND external_subject=$2")
            .bind(ISSUER).bind(&fixture_subject).execute(&pool).await.expect("fixture access identity removed last");
    }
    let directory = std::env::temp_dir().join(format!("litecr-reconciliation-{suffix}"));
    if directory.exists() {
        fs::remove_dir_all(directory).expect("fixture staging directory removed");
    }
    pool.close().await;
}

fn ensure(condition: bool, message: &str) -> Result<(), String> {
    condition.then_some(()).ok_or_else(|| message.to_owned())
}
