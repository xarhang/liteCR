use async_trait::async_trait;
use litecr_application::{
    access::{
        AccessService, AccessServiceConfig, AccessSessionRepository, ActorKind, ActorRef,
        OidcAuthorization, OidcFlow, OidcIdentityClaims, OidcProvider, OidcProviderError,
        Permission, ResourceScope, authorize,
    },
    matching::MatchingStore,
    migration::{
        MigrationBatch, MigrationError, MigrationExecutor, MigrationPackagePolicy, MigrationRecord,
        MigrationRun, MigrationService, QuarantineCorrection, QuarantineDisposition,
        QuarantineDispositionAction, ReverseManifest, ReversePreflightRequest,
    },
    registry::{RegistryService, RegistryStore, SourceAssertion, SourceRecordCommand},
};
use litecr_infrastructure::{
    access::{
        PostgresAccessAuthenticator, PostgresAccessDirectory, PostgresAccessSessionRepository,
        PostgresOidcFlowStore,
    },
    connect,
    matching::PostgresMatchingStore,
    migrate,
    migration::{PostgresMigrationExecutor, PostgresMigrationJournal},
    registry::PostgresRegistryStore,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};
use std::{sync::Arc, time::SystemTime};

const SYNTHETIC_ISSUER: &str = "https://litecr-migration-test.invalid";

fn suffix() -> String {
    format!(
        "{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("system clock after epoch")
            .as_nanos()
    )
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

fn hex(bytes: &[u8]) -> String {
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        write!(encoded, "{byte:02x}").expect("writing into a String cannot fail");
    }
    encoded
}

fn record(row: Value) -> MigrationRecord {
    let raw = serde_json::to_vec(&row).expect("synthetic row serializes");
    let source_system_id = row["source_system_id"].as_str().unwrap().to_owned();
    let source_record_key = row["source_record_key"].as_str().unwrap().to_owned();
    let source_version = row["source_version"].as_str().unwrap().to_owned();
    let canonical = serde_json::to_vec(&row).expect("canonical row serializes");
    MigrationRecord {
        source_system_id,
        source_record_key,
        source_version,
        payload_sha256: sha256(&canonical),
        raw_ndjson: [raw, b"\n".to_vec()].concat(),
    }
}

fn batch(run: MigrationRun, sequence: u64, records: Vec<MigrationRecord>) -> MigrationBatch {
    let exact_bytes = records
        .iter()
        .flat_map(|record| record.raw_ndjson.iter().copied())
        .collect::<Vec<_>>();
    let line_count = records.len() as u64;
    MigrationBatch {
        run,
        sequence,
        first_input_line: 1,
        last_input_line: line_count,
        batch_sha256: sha256(&exact_bytes),
        exact_bytes,
        records,
    }
}

fn run(key: &str, registry_id: &str, organization_id: &str, count: u64) -> MigrationRun {
    MigrationRun {
        manifest_sha256: sha256(format!("manifest-{key}").as_bytes()),
        stream_sha256: sha256(format!("stream-{key}").as_bytes()),
        snapshot_id: format!("synthetic-snapshot-{key}"),
        source_system_id: "synthetic-source".to_owned(),
        registry_id: registry_id.to_owned(),
        organization_id: organization_id.to_owned(),
        expected_record_count: count,
    }
}

fn supported_row(
    source_system_id: &str,
    source_record_key: &str,
    public_id: &str,
    cruid: &str,
    alias: &str,
    identifier: &str,
) -> Value {
    json!({
        "source_system_id": source_system_id,
        "source_record_key": source_record_key,
        "source_version": "v1",
        "public_id": public_id,
        "cruid": cruid,
        "aliases": [alias],
        "assertions": [
            {"field_path":"name.original","original_value":"ຄຳ ທົດສອບ","language_tag":"lo","script":"Lao"},
            {"field_path":"birthDate","original_value":"1984-06","partial_date_precision":"month"}
        ],
        "identifiers": [{"authority":"urn:litecr:test:clinic","type":"MRN","value":identifier,"authoritative":true}]
    })
}

#[tokio::test]
async fn postgres_migration_import_retry_rollback_and_digest_guard() {
    let database_url = std::env::var("LITECR_TEST_DATABASE_URL")
        .expect("set LITECR_TEST_DATABASE_URL to the serialized synthetic-only PostgreSQL DB");
    let key = suffix();
    let result = tokio::spawn(run_scenario(database_url.clone(), key.clone())).await;
    cleanup(&database_url, &key).await;
    assert!(
        result.is_ok(),
        "migration scenario task panicked: {result:?}"
    );
    result.unwrap().expect("migration PostgreSQL scenario");
}

#[tokio::test]
async fn postgres_server_verified_package_v2_stages_exact_bytes_and_applies_atomically() {
    let database_url = std::env::var("LITECR_TEST_DATABASE_URL")
        .expect("set LITECR_TEST_DATABASE_URL to the serialized synthetic-only PostgreSQL DB");
    let key = suffix();
    let result = tokio::spawn(run_v2_scenario(database_url.clone(), key.clone())).await;
    cleanup(&database_url, &key).await;
    assert!(result.is_ok(), "v2 scenario task panicked: {result:?}");
    result
        .unwrap()
        .expect("server-verified v2 PostgreSQL scenario");
}

async fn run_v2_scenario(database_url: String, key: String) -> Result<(), String> {
    let pool = connect(&database_url, 8)
        .await
        .map_err(|_| "synthetic v2 database connection available")?;
    migrate(&pool)
        .await
        .map_err(|_| "schema 20 migrations apply")?;
    let registry_a = format!("migration-reg-a-{key}");
    let organization_a = format!("migration-org-a-{key}");
    let registry_b = format!("migration-reg-b-{key}");
    let organization_b = format!("migration-org-b-{key}");
    let (access, _) = provisioned_access(
        &pool,
        &key,
        &registry_a,
        &organization_a,
        &registry_b,
        &organization_b,
    )
    .await?;
    let row = supported_row(
        "synthetic-source",
        &format!("v2-source-{key}"),
        &format!("v2-public-{key}"),
        &format!("v2-cruid-{key}"),
        &format!("v2-alias-{key}"),
        &format!("v2-mrn-{key}"),
    );
    let mut stream = serde_json::to_vec(&row).map_err(|_| "synthetic package row serializes")?;
    stream.push(b'\n');
    let manifest = serde_json::to_vec(&json!({
        "format": litecr_application::migration::PACKAGE_FORMAT,
        "snapshot_id": format!("v2-snapshot-{key}"),
        "source_system_id": "synthetic-source",
        "target_registry_id": registry_a,
        "target_organization_id": organization_a,
        "exported_at": "2026-10-10T00:00:00Z",
        "record_count": 1,
        "stream_bytes": stream.len(),
        "stream_sha256": hex(&sha256(&stream)),
    }))
    .map_err(|_| "exact v2 manifest serializes")?;
    let policy = MigrationPackagePolicy {
        global_quota_bytes: 1024 * 1024,
        max_active_uploads: 4,
        retention_seconds: 3600,
        storage_protection_reference: "synthetic-test-policy".into(),
    };
    let service = MigrationService::new(Box::new(PostgresMigrationExecutor::new(pool.clone())));
    let created = service
        .create_package_v2(Some(&access), &manifest, &policy)
        .await
        .map_err(|error| format!("create exact v2 upload: {error:?}"))?;
    ensure(
        created.state == "receiving",
        "new v2 upload awaits complete exact stream",
    )?;

    let (left, right) = tokio::join!(
        service.stage_package_v2_batch(Some(&access), &created.upload_id, 1, &stream, &policy),
        service.stage_package_v2_batch(Some(&access), &created.upload_id, 1, &stream, &policy),
    );
    let staged = left.map_err(|error| format!("concurrent exact retry succeeds: {error:?}"))?;
    let retry = right.map_err(|error| format!("concurrent duplicate converges: {error:?}"))?;
    ensure(
        staged.staged_batch_count == 1 && retry.staged_bytes == stream.len() as u64,
        "concurrent byte-identical staging produces one durable batch",
    )?;
    let imported_before_finalize: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM litecr.migration_journal WHERE manifest_sha256=$1",
    )
    .bind(created.manifest_sha256.to_vec())
    .fetch_one(&pool)
    .await
    .map_err(|_| "pre-finalization source write count is queryable")?;
    ensure(
        imported_before_finalize == 0,
        "staging alone does not mutate registry records",
    )?;
    let mut changed = stream.clone();
    changed.insert(0, b' ');
    ensure(
        matches!(
            service
                .stage_package_v2_batch(Some(&access), &created.upload_id, 1, &changed, &policy)
                .await,
            Err(MigrationError::PackageConflict)
        ),
        "different exact bytes cannot reuse a staged sequence",
    )?;
    ensure(
        matches!(
            service
                .stage_package_v2_batch(Some(&access), &created.upload_id, 3, &stream, &policy)
                .await,
            Err(MigrationError::PackageConflict)
        ),
        "batch gaps are rejected",
    )?;
    let ready = service
        .finalize_package_v2(Some(&access), &created.upload_id, &policy)
        .await
        .map_err(|error| format!("server verifies exact package commitment: {error:?}"))?;
    ensure(
        ready.state == "ready",
        "only exact finalized stream reaches ready",
    )?;
    let applied = service
        .apply_package_v2_batch(Some(&access), &created.upload_id)
        .await
        .map_err(|error| format!("v2 executor applies finalized server-held bytes: {error:?}"))?;
    ensure(
        applied.state == "completed"
            && applied.committed_batch_sequence == 1
            && applied.committed_record_count == 1,
        "package receipt and migration checkpoint complete together",
    )?;
    let exact_payload: Vec<u8> = sqlx::query_scalar(
        "SELECT original_payload_ndjson FROM litecr.migration_journal WHERE manifest_sha256=$1",
    )
    .bind(applied.manifest_sha256.to_vec())
    .fetch_one(&pool)
    .await
    .map_err(|_| "v2 source journal row is durable")?;
    ensure(
        exact_payload == stream,
        "journal preserves exact original NDJSON bytes",
    )?;
    let staged_batches: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM litecr.migration_package_v2_batch WHERE upload_id=$1::uuid",
    )
    .bind(&created.upload_id)
    .fetch_one(&pool)
    .await
    .map_err(|_| "completed temporary stage count is queryable")?;
    ensure(
        staged_batches == 0,
        "completed apply purges temporary raw staged batches",
    )?;
    let repeated = service
        .apply_package_v2_batch(Some(&access), &created.upload_id)
        .await
        .map_err(|error| format!("completed apply retry returns durable receipt: {error:?}"))?;
    ensure(
        repeated.state == "completed" && repeated.committed_record_count == 1,
        "retry after commit returns the same aggregate package receipt",
    )?;

    // A valid row with different exact bytes must be rejected by the server's
    // whole-stream verifier, and PHI-bearing temporary batch bytes are purged.
    let rejected_manifest = serde_json::to_vec(&json!({
        "format": litecr_application::migration::PACKAGE_FORMAT,
        "snapshot_id": format!("v2-rejected-snapshot-{key}"),
        "source_system_id": "synthetic-source",
        "target_registry_id": registry_a,
        "target_organization_id": organization_a,
        "exported_at": "2026-10-10T00:00:00Z",
        "record_count": 1,
        "stream_bytes": stream.len(),
        "stream_sha256": hex(&sha256(&stream)),
    }))
    .map_err(|_| "rejection-test manifest serializes")?;
    let rejected = service
        .create_package_v2(Some(&access), &rejected_manifest, &policy)
        .await
        .map_err(|error| format!("create stream-integrity test package: {error:?}"))?;
    let mut different_stream = stream.clone();
    let needle = format!("v2-alias-{key}").into_bytes();
    let offset = different_stream
        .windows(needle.len())
        .position(|window| window == needle)
        .ok_or("synthetic alias appears in exact row bytes")?;
    different_stream[offset] = if different_stream[offset] == b'v' {
        b'w'
    } else {
        b'v'
    };
    service
        .stage_package_v2_batch(
            Some(&access),
            &rejected.upload_id,
            1,
            &different_stream,
            &policy,
        )
        .await
        .map_err(|error| format!("stage structurally valid but wrong stream: {error:?}"))?;
    ensure(
        matches!(
            service
                .finalize_package_v2(Some(&access), &rejected.upload_id, &policy)
                .await,
            Err(MigrationError::PackageVerificationFailed)
        ),
        "server rejects rows that do not match the exact manifest stream digest",
    )?;
    let rejected_receipt = service
        .inspect_package_v2(Some(&access), &rejected.upload_id)
        .await
        .map_err(|error| format!("rejected v2 receipt remains inspectable: {error:?}"))?;
    ensure(
        rejected_receipt.state == "rejected" && rejected_receipt.staged_bytes == 0,
        "rejection clears staged-byte quota",
    )?;
    let rejected_reservation: i64 = sqlx::query_scalar(
        "SELECT reserved_bytes FROM litecr.migration_package_v2 WHERE upload_id=$1::uuid",
    )
    .bind(&rejected.upload_id)
    .fetch_one(&pool)
    .await
    .map_err(|_| "rejected package reservation remains inspectable")?;
    ensure(
        rejected_reservation == 0,
        "rejection releases the whole-package reservation",
    )?;
    let rejected_batches: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM litecr.migration_package_v2_batch WHERE upload_id=$1::uuid",
    )
    .bind(&rejected.upload_id)
    .fetch_one(&pool)
    .await
    .map_err(|_| "rejected raw batch count is queryable")?;
    ensure(rejected_batches == 0, "rejected raw batch bytes are purged")?;
    let rejected_retry = service
        .create_package_v2(Some(&access), &rejected_manifest, &policy)
        .await
        .map_err(|error| format!("rejected package retry returns terminal receipt: {error:?}"))?;
    ensure(
        rejected_retry.upload_id == rejected.upload_id && rejected_retry.state == "rejected",
        "rejected package identity and terminal state remain immutable on retry",
    )?;

    let reservation_manifest = |snapshot_id: &str| {
        serde_json::to_vec(&json!({
            "format": litecr_application::migration::PACKAGE_FORMAT,
            "snapshot_id": snapshot_id,
            "source_system_id": "synthetic-source",
            "target_registry_id": registry_a,
            "target_organization_id": organization_a,
            "exported_at": "2026-10-10T00:00:00Z",
            "record_count": 1,
            "stream_bytes": stream.len(),
            "stream_sha256": hex(&sha256(&stream)),
        }))
    };
    let too_small_manifest = reservation_manifest(&format!("v2-too-large-{key}"))
        .map_err(|_| "too-large package manifest serializes")?;
    let too_small_policy = MigrationPackagePolicy {
        global_quota_bytes: stream.len() as u64 - 1,
        max_active_uploads: 4,
        retention_seconds: 3600,
        storage_protection_reference: "synthetic-test-policy".into(),
    };
    ensure(
        matches!(
            service
                .create_package_v2(Some(&access), &too_small_manifest, &too_small_policy)
                .await,
            Err(MigrationError::PackageQuotaExceeded)
        ),
        "package larger than available staging quota is rejected before upload creation",
    )?;

    let reservation_policy = MigrationPackagePolicy {
        global_quota_bytes: stream.len() as u64,
        max_active_uploads: 4,
        retention_seconds: 3600,
        storage_protection_reference: "synthetic-test-policy".into(),
    };
    let first_manifest = reservation_manifest(&format!("v2-reserve-a-{key}"))
        .map_err(|_| "first reservation manifest serializes")?;
    let second_manifest = reservation_manifest(&format!("v2-reserve-b-{key}"))
        .map_err(|_| "second reservation manifest serializes")?;
    let (first_reservation, second_reservation) = tokio::join!(
        service.create_package_v2(Some(&access), &first_manifest, &reservation_policy),
        service.create_package_v2(Some(&access), &second_manifest, &reservation_policy),
    );
    let accepted_reservation = match (first_reservation, second_reservation) {
        (Ok(upload), Err(MigrationError::PackageQuotaExceeded))
        | (Err(MigrationError::PackageQuotaExceeded), Ok(upload)) => upload,
        other => {
            return Err(format!(
                "concurrent package reservations serialize safely: {other:?}"
            ));
        }
    };
    let reserved_bytes: i64 = sqlx::query_scalar(
        "SELECT COALESCE(sum(reserved_bytes),0)::bigint FROM litecr.migration_package_v2 WHERE state IN ('receiving','verifying','ready','applying')",
    )
    .fetch_one(&pool)
    .await
    .map_err(|_| "reserved staging byte total is queryable")?;
    ensure(
        reserved_bytes == stream.len() as i64,
        "global reservation accounts for the complete declared package before staging",
    )?;

    let expiry_retry_row = supported_row(
        "synthetic-source",
        &format!("v2-expiry-retry-source-{key}"),
        &format!("v2-expiry-retry-public-{key}"),
        &format!("v2-expiry-retry-cruid-{key}"),
        &format!("v2-expiry-retry-alias-{key}"),
        &format!("v2-expiry-retry-mrn-{key}"),
    );
    let expiry_retry_stream = [
        serde_json::to_vec(&expiry_retry_row).map_err(|_| "expiry retry row serializes")?,
        b"\n".to_vec(),
    ]
    .concat();
    let expiring_manifest = serde_json::to_vec(&json!({
        "format": litecr_application::migration::PACKAGE_FORMAT,
        "snapshot_id": format!("v2-expiring-snapshot-{key}"),
        "source_system_id": "synthetic-source",
        "target_registry_id": registry_a,
        "target_organization_id": organization_a,
        "exported_at": "2026-10-10T00:00:00Z",
        "record_count": 1,
        "stream_bytes": expiry_retry_stream.len(),
        "stream_sha256": hex(&sha256(&expiry_retry_stream)),
    }))
    .map_err(|_| "expiry-test manifest serializes")?;
    let expiring = service
        .create_package_v2(Some(&access), &expiring_manifest, &policy)
        .await
        .map_err(|error| format!("create expiry test package: {error:?}"))?;
    service
        .stage_package_v2_batch(
            Some(&access),
            &expiring.upload_id,
            1,
            &expiry_retry_stream,
            &policy,
        )
        .await
        .map_err(|error| format!("stage expiry cleanup package: {error:?}"))?;
    sqlx::query("UPDATE litecr.migration_package_v2 SET expires_at=now()-interval '1 second' WHERE upload_id=$1::uuid")
        .bind(&expiring.upload_id).execute(&pool).await
        .map_err(|_| "synthetic test package expiry set")?;
    let cleaned = PostgresMigrationExecutor::new(pool.clone())
        .cleanup_expired_packages_v2(1)
        .await
        .map_err(|error| format!("bounded expiry cleanup succeeds: {error:?}"))?;
    ensure(
        cleaned == 1,
        "operator cleanup purges one expired package in a bounded unit",
    )?;
    let expired = service
        .inspect_package_v2(Some(&access), &expiring.upload_id)
        .await
        .map_err(|error| format!("expired v2 receipt remains inspectable: {error:?}"))?;
    ensure(
        expired.state == "expired" && expired.staged_bytes == 0,
        "expired package releases quota and transitions to terminal state",
    )?;
    let reservation_after_cleanup: i64 = sqlx::query_scalar(
        "SELECT reserved_bytes FROM litecr.migration_package_v2 WHERE upload_id=$1::uuid",
    )
    .bind(&expiring.upload_id)
    .fetch_one(&pool)
    .await
    .map_err(|_| "expired package reservation remains inspectable")?;
    ensure(
        reservation_after_cleanup == 0,
        "expiry cleanup releases the full pre-reserved package quota",
    )?;
    let partial_reservation: i64 = sqlx::query_scalar(
        "SELECT reserved_bytes FROM litecr.migration_package_v2 WHERE upload_id=$1::uuid",
    )
    .bind(&accepted_reservation.upload_id)
    .fetch_one(&pool)
    .await
    .map_err(|_| "active upload reservation remains inspectable")?;
    ensure(
        partial_reservation == stream.len() as i64,
        "cleanup does not release quota held by an unexpired active package",
    )?;

    let retry_quota_policy = MigrationPackagePolicy {
        global_quota_bytes: stream.len() as u64,
        max_active_uploads: 4,
        retention_seconds: 3600,
        storage_protection_reference: "synthetic-test-policy".into(),
    };
    ensure(
        matches!(
            service
                .create_package_v2(Some(&access), &expiring_manifest, &retry_quota_policy)
                .await,
            Err(MigrationError::PackageQuotaExceeded)
        ),
        "expired package stays terminal when retry cannot reserve global quota",
    )?;
    let retry_capacity_policy = MigrationPackagePolicy {
        global_quota_bytes: 1024 * 1024,
        max_active_uploads: 1,
        retention_seconds: 3600,
        storage_protection_reference: "synthetic-test-policy".into(),
    };
    ensure(
        matches!(
            service
                .create_package_v2(Some(&access), &expiring_manifest, &retry_capacity_policy)
                .await,
            Err(MigrationError::PackageQuotaExceeded)
        ),
        "expired package stays terminal when retry cannot reserve active-upload capacity",
    )?;
    let retry_policy = MigrationPackagePolicy {
        global_quota_bytes: policy.global_quota_bytes,
        max_active_uploads: policy.max_active_uploads,
        retention_seconds: policy.retention_seconds,
        storage_protection_reference: "synthetic-retry-protection-policy".into(),
    };
    let reopened = service
        .create_package_v2(Some(&access), &expiring_manifest, &retry_policy)
        .await
        .map_err(|error| format!("reopen identical expired package after cleanup: {error:?}"))?;
    ensure(
        reopened.upload_id == expiring.upload_id
            && reopened.state == "receiving"
            && reopened.staged_batch_count == 0
            && reopened.staged_bytes == 0
            && reopened.record_count == 0,
        "eligible expired package reuses its identity with clean receiving counters",
    )?;
    let reopened_storage: (i64, bool, String) = sqlx::query_as(
        "SELECT reserved_bytes,expires_at > now(),storage_protection_reference FROM litecr.migration_package_v2 WHERE upload_id=$1::uuid",
    )
    .bind(&reopened.upload_id)
    .fetch_one(&pool)
    .await
    .map_err(|_| "reopened reservation and expiry are queryable")?;
    ensure(
        reopened_storage
            == (
                expiry_retry_stream.len() as i64,
                true,
                retry_policy.storage_protection_reference.clone(),
            ),
        "reopened package holds full quota, fresh expiry, and the active protection reference",
    )?;
    service
        .stage_package_v2_batch(
            Some(&access),
            &reopened.upload_id,
            1,
            &expiry_retry_stream,
            &retry_policy,
        )
        .await
        .map_err(|error| format!("restage identical package after expiry: {error:?}"))?;
    service
        .finalize_package_v2(Some(&access), &reopened.upload_id, &retry_policy)
        .await
        .map_err(|error| format!("verify restaged package after expiry: {error:?}"))?;
    let retried_apply = service
        .apply_package_v2_batch(Some(&access), &reopened.upload_id)
        .await
        .map_err(|error| format!("apply reopened package exactly once: {error:?}"))?;
    ensure(
        retried_apply.state == "completed" && retried_apply.committed_record_count == 1,
        "reopened package can finish its durable apply once",
    )?;
    let retried_journal_rows: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM litecr.migration_journal WHERE manifest_sha256=$1",
    )
    .bind(sha256(&expiring_manifest).to_vec())
    .fetch_one(&pool)
    .await
    .map_err(|_| "retried package journal count is queryable")?;
    ensure(
        retried_journal_rows == 1,
        "identical expired-package retry records one durable import journal entry",
    )?;
    let completed_retry = service
        .create_package_v2(Some(&access), &expiring_manifest, &retry_policy)
        .await
        .map_err(|error| format!("completed package retry returns durable receipt: {error:?}"))?;
    ensure(
        completed_retry.upload_id == reopened.upload_id
            && completed_retry.state == "completed"
            && completed_retry.committed_record_count == 1,
        "completed package identity and terminal state remain immutable on retry",
    )?;

    let first_partial_row = supported_row(
        "synthetic-source",
        &format!("v2-partial-first-source-{key}"),
        &format!("v2-partial-first-public-{key}"),
        &format!("v2-partial-first-cruid-{key}"),
        &format!("v2-partial-first-alias-{key}"),
        &format!("v2-partial-first-mrn-{key}"),
    );
    let first_partial_bytes = [
        serde_json::to_vec(&first_partial_row).map_err(|_| "first partial row serializes")?,
        b"\n".to_vec(),
    ]
    .concat();
    let second_row = supported_row(
        "synthetic-source",
        &format!("v2-partial-source-{key}"),
        &format!("v2-partial-public-{key}"),
        &format!("v2-partial-cruid-{key}"),
        &format!("v2-partial-alias-{key}"),
        &format!("v2-partial-mrn-{key}"),
    );
    let mut partial_stream = first_partial_bytes.clone();
    let split_at = partial_stream.len();
    let second_bytes = [
        serde_json::to_vec(&second_row).map_err(|_| "partial-apply row serializes")?,
        b"\n".to_vec(),
    ]
    .concat();
    partial_stream.extend_from_slice(&second_bytes);
    let partial_manifest = serde_json::to_vec(&json!({
        "format": litecr_application::migration::PACKAGE_FORMAT,
        "snapshot_id": format!("v2-partial-snapshot-{key}"),
        "source_system_id": "synthetic-source",
        "target_registry_id": registry_a,
        "target_organization_id": organization_a,
        "exported_at": "2026-10-10T00:00:00Z",
        "record_count": 2,
        "stream_bytes": partial_stream.len(),
        "stream_sha256": hex(&sha256(&partial_stream)),
    }))
    .map_err(|_| "partial-apply manifest serializes")?;
    let partial = service
        .create_package_v2(Some(&access), &partial_manifest, &policy)
        .await
        .map_err(|error| format!("create partial-apply test package: {error:?}"))?;
    service
        .stage_package_v2_batch(
            Some(&access),
            &partial.upload_id,
            1,
            &partial_stream[..split_at],
            &policy,
        )
        .await
        .map_err(|error| format!("stage partial-apply first batch: {error:?}"))?;
    service
        .stage_package_v2_batch(
            Some(&access),
            &partial.upload_id,
            2,
            &partial_stream[split_at..],
            &policy,
        )
        .await
        .map_err(|error| format!("stage partial-apply second batch: {error:?}"))?;
    service
        .finalize_package_v2(Some(&access), &partial.upload_id, &policy)
        .await
        .map_err(|error| format!("finalize partial-apply package: {error:?}"))?;
    let first_apply = service
        .apply_package_v2_batch(Some(&access), &partial.upload_id)
        .await
        .map_err(|error| format!("apply partial package prefix: {error:?}"))?;
    ensure(
        first_apply.state == "applying",
        "first batch leaves a resumable applying state",
    )?;
    sqlx::query("UPDATE litecr.migration_package_v2 SET expires_at=now()-interval '1 second' WHERE upload_id=$1::uuid")
        .bind(&partial.upload_id).execute(&pool).await
        .map_err(|_| "partial package synthetic expiry set")?;
    let cleaned = PostgresMigrationExecutor::new(pool.clone())
        .cleanup_expired_packages_v2(100)
        .await
        .map_err(|error| format!("bounded cleanup runs around partial apply: {error:?}"))?;
    ensure(
        cleaned == 0,
        "expiry cleanup never purges an applying package",
    )?;
    let retained_batch_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM litecr.migration_package_v2_batch WHERE upload_id=$1::uuid",
    )
    .bind(&partial.upload_id)
    .fetch_one(&pool)
    .await
    .map_err(|_| "partial applying staged batch count is queryable")?;
    ensure(
        retained_batch_count == 2,
        "partial apply retains both exact batch byte sequences",
    )?;
    let resumed = service
        .apply_package_v2_batch(Some(&access), &partial.upload_id)
        .await
        .map_err(|error| {
            format!("expired partial apply resumes from durable checkpoint: {error:?}")
        })?;
    ensure(
        resumed.state == "completed",
        "partial apply resumes safely after its TTL",
    )?;
    pool.close().await;
    Ok(())
}

async fn run_scenario(database_url: String, key: String) -> Result<(), String> {
    let pool = connect(&database_url, 8)
        .await
        .map_err(|_| "synthetic migration database connection failed")?;
    migrate(&pool)
        .await
        .map_err(|_| "synthetic migration database schema setup failed")?;

    let registry_a = format!("migration-reg-a-{key}");
    let organization_a = format!("migration-org-a-{key}");
    let registry_b = format!("migration-reg-b-{key}");
    let organization_b = format!("migration-org-b-{key}");
    let (access, subject) = provisioned_access(
        &pool,
        &key,
        &registry_a,
        &organization_a,
        &registry_b,
        &organization_b,
    )
    .await?;
    let scope = ResourceScope {
        registry_id: registry_a.clone(),
        organization_id: organization_a.clone(),
        record_id: None,
    };
    authorize(Some(&access), Permission::SourceWrite, &scope)
        .map_err(|_| "ordinary source-write behavior should remain available")?;
    authorize(Some(&access), Permission::MigrationWrite, &scope)
        .map_err(|_| "explicit migration grant should authorize its synthetic scope")?;

    let executor = PostgresMigrationExecutor::new(pool.clone());
    let source_system = "synthetic-source";
    let source_key = format!("source-record-{key}");
    let public_id = format!("public-{key}");
    let cruid = format!("cruid-{key}");
    let alias = format!("alias-{key}");
    let identifier_value = format!("mrn-{key}");
    let source_row = supported_row(
        source_system,
        &source_key,
        &public_id,
        &cruid,
        &alias,
        &identifier_value,
    );
    let first_batch = batch(
        run(&format!("primary-{key}"), &registry_a, &organization_a, 1),
        1,
        vec![record(source_row.clone())],
    );
    let migration_service =
        MigrationService::new(Box::new(PostgresMigrationExecutor::new(pool.clone())));
    let disposition = QuarantineDisposition {
        action: QuarantineDispositionAction::CorrectedResubmission,
        parent_manifest_sha256: sha256(format!("quarantine-parent-{key}").as_bytes()),
        replacement_manifest_sha256: Some(sha256(
            format!("quarantine-replacement-{key}").as_bytes(),
        )),
        registry_id: registry_a.clone(),
        organization_id: organization_a.clone(),
        reason: "synthetic mechanically recoverable row correction".into(),
        idempotency_key: format!("quarantine-disposition-{key}"),
        corrections: vec![QuarantineCorrection {
            input_line: 2,
            duplicate_of_line: None,
            quarantine_code: "ROW_INVALID_UTF8_OR_JSON".into(),
            original_payload_sha256: sha256(b"invalid synthetic bytes\n"),
            corrected_payload_sha256: Some(sha256(b"corrected synthetic row\n")),
        }],
    };
    let disposition_receipt = migration_service
        .record_quarantine_disposition(Some(&access), disposition.clone())
        .await
        .map_err(|error| format!("reviewed synthetic disposition records: {error:?}"))?;
    ensure(
        !disposition_receipt.replayed,
        "new disposition is not a replay",
    )?;
    ensure(
        disposition_receipt.correction_count == 1,
        "one row correction is recorded",
    )?;
    let retry_receipt = migration_service
        .record_quarantine_disposition(Some(&access), disposition.clone())
        .await
        .map_err(|error| format!("identical disposition retries idempotently: {error:?}"))?;
    ensure(
        retry_receipt.replayed,
        "identical idempotency retry returns replay receipt",
    )?;
    ensure(
        retry_receipt.disposition_id == disposition_receipt.disposition_id,
        "disposition retry returns the same durable receipt",
    )?;
    let audited: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM litecr.migration_quarantine_disposition d JOIN litecr.migration_quarantine_disposition_item i USING (disposition_id) JOIN litecr.mutation_audit a ON a.resource_ref=d.disposition_id::text AND a.action='migration.quarantine.disposition' JOIN litecr.outbox_event o ON o.aggregate_id=d.disposition_id AND o.event_type='migration.quarantine.disposition' WHERE d.disposition_id=$1::uuid AND i.input_line=2 AND i.quarantine_code='ROW_INVALID_UTF8_OR_JSON' AND a.actor_id=$2 AND d.registry_id=$3 AND d.organization_id=$4")
        .bind(&disposition_receipt.disposition_id)
        .bind(access.actor().id.as_str())
        .bind(&registry_a)
        .bind(&organization_a)
        .fetch_one(&pool)
        .await
        .map_err(|_| "disposition, item, audit and outbox rows queried")?;
    ensure(
        audited == 1,
        "disposition and audit/outbox item are committed together",
    )?;
    let mut conflicting_retry = disposition.clone();
    conflicting_retry.reason = "different reason under reused key".into();
    ensure(
        matches!(
            migration_service
                .record_quarantine_disposition(Some(&access), conflicting_retry)
                .await,
            Err(MigrationError::CheckpointConflict)
        ),
        "idempotency key cannot be reused for changed disposition input",
    )?;
    let relationship_correction = QuarantineDisposition {
        action: QuarantineDispositionAction::CorrectedResubmission,
        parent_manifest_sha256: sha256(b"relationship-parent"),
        replacement_manifest_sha256: Some(sha256(b"relationship-replacement")),
        registry_id: registry_a.clone(),
        organization_id: organization_a.clone(),
        reason: "unsupported relationship is not safely importable".into(),
        idempotency_key: format!("blocked-relationship-{key}"),
        corrections: vec![QuarantineCorrection {
            input_line: 3,
            duplicate_of_line: None,
            quarantine_code: "HISTORICAL_RELATIONSHIPS_UNSUPPORTED".into(),
            original_payload_sha256: sha256(b"relationship source"),
            corrected_payload_sha256: Some(sha256(b"relationship stripped")),
        }],
    };
    ensure(
        matches!(
            migration_service
                .record_quarantine_disposition(Some(&access), relationship_correction)
                .await,
            Err(MigrationError::InvalidDisposition)
        ),
        "unsupported historical relationships cannot be dispositioned into import",
    )?;
    let deferred_receipt = migration_service
        .record_quarantine_disposition(
            Some(&access),
            QuarantineDisposition {
                action: QuarantineDispositionAction::Deferred,
                parent_manifest_sha256: sha256(format!("deferred-relationship-{key}").as_bytes()),
                replacement_manifest_sha256: None,
                registry_id: registry_a.clone(),
                organization_id: organization_a.clone(),
                reason: "relationship model is unsupported; retain for owner review".into(),
                idempotency_key: format!("deferred-relationship-{key}"),
                corrections: vec![QuarantineCorrection {
                    input_line: 3,
                    duplicate_of_line: None,
                    quarantine_code: "HISTORICAL_RELATIONSHIPS_UNSUPPORTED".into(),
                    original_payload_sha256: sha256(b"relationship source"),
                    corrected_payload_sha256: None,
                }],
            },
        )
        .await
        .map_err(|error| format!("unsupported relationship is auditable as deferred: {error:?}"))?;
    ensure(
        deferred_receipt.replacement_manifest_sha256.is_none(),
        "deferred relationship has no importable replacement manifest",
    )?;
    let duplicate_hash = sha256(b"exact duplicate physical row\n");
    let duplicate_receipt = migration_service
        .record_quarantine_disposition(
            Some(&access),
            QuarantineDisposition {
                action: QuarantineDispositionAction::DuplicateAcknowledged,
                parent_manifest_sha256: sha256(format!("duplicate-parent-{key}").as_bytes()),
                replacement_manifest_sha256: Some(sha256(
                    format!("duplicate-replacement-{key}").as_bytes(),
                )),
                registry_id: registry_a.clone(),
                organization_id: organization_a.clone(),
                reason: "verified byte-identical physical replay".into(),
                idempotency_key: format!("duplicate-ack-{key}"),
                corrections: vec![QuarantineCorrection {
                    input_line: 5,
                    duplicate_of_line: Some(2),
                    quarantine_code: "EXACT_REPLAY_DUPLICATE".into(),
                    original_payload_sha256: duplicate_hash,
                    corrected_payload_sha256: Some(duplicate_hash),
                }],
            },
        )
        .await
        .map_err(|error| format!("exact duplicate disposition is recorded: {error:?}"))?;
    let duplicate_item_valid: bool = sqlx::query_scalar("SELECT duplicate_of_line=2 AND corrected_payload_sha256=original_payload_sha256 FROM litecr.migration_quarantine_disposition_item WHERE disposition_id=$1::uuid AND input_line=5 AND quarantine_code='EXACT_REPLAY_DUPLICATE'")
        .bind(&duplicate_receipt.disposition_id)
        .fetch_one(&pool)
        .await
        .map_err(|_| "duplicate provenance hashes queried")?;
    ensure(
        duplicate_item_valid,
        "duplicate acknowledgement persists exact source line and equal row hashes",
    )?;
    let combined_hash = sha256(b"combined exact replay physical row\n");
    let combined_receipt = migration_service
        .record_quarantine_disposition(
            Some(&access),
            QuarantineDisposition {
                action: QuarantineDispositionAction::CorrectedResubmission,
                parent_manifest_sha256: sha256(format!("combined-parent-{key}").as_bytes()),
                replacement_manifest_sha256: Some(sha256(
                    format!("combined-replacement-{key}").as_bytes(),
                )),
                registry_id: registry_a.clone(),
                organization_id: organization_a.clone(),
                reason: "mechanical repair plus exact replay removal".into(),
                idempotency_key: format!("combined-disposition-{key}"),
                corrections: vec![
                    QuarantineCorrection {
                        input_line: 1,
                        duplicate_of_line: None,
                        quarantine_code: "ROW_INVALID_UTF8_OR_JSON".into(),
                        original_payload_sha256: sha256(b"invalid combined row\n"),
                        corrected_payload_sha256: Some(sha256(b"corrected combined row\n")),
                    },
                    QuarantineCorrection {
                        input_line: 4,
                        duplicate_of_line: Some(1),
                        quarantine_code: "EXACT_REPLAY_DUPLICATE".into(),
                        original_payload_sha256: combined_hash,
                        corrected_payload_sha256: Some(combined_hash),
                    },
                ],
            },
        )
        .await
        .map_err(|error| {
            format!("mixed correction and replay disposition is accepted: {error:?}")
        })?;
    ensure(
        combined_receipt.correction_count == 2,
        "combined disposition records both separately evidenced line actions",
    )?;
    migration_service
        .validate_batch(Some(&access), &first_batch)
        .await
        .map_err(|error| format!("supported batch passes no-write validation: {error:?}"))?;
    let mut unsupported_row = supported_row(
        source_system,
        &format!("unsupported-validation-{key}"),
        &format!("unsupported-validation-public-{key}"),
        &format!("unsupported-validation-cruid-{key}"),
        &format!("unsupported-validation-alias-{key}"),
        &format!("unsupported-validation-mrn-{key}"),
    );
    unsupported_row["relationships"] = json!([{"type":"historical-link"}]);
    let unsupported_batch = batch(
        run(
            &format!("unsupported-validation-{key}"),
            &registry_a,
            &organization_a,
            1,
        ),
        1,
        vec![record(unsupported_row)],
    );
    ensure(
        matches!(
            migration_service
                .validate_batch(Some(&access), &unsupported_batch)
                .await,
            Err(MigrationError::UnsupportedRecord)
        ),
        "unsupported historical relationship is refused during no-write validation",
    )?;
    let validation_side_effects: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM litecr.migration_checkpoint WHERE manifest_sha256=$1 OR manifest_sha256=$2",
    )
    .bind(first_batch.run.manifest_sha256.to_vec())
    .bind(unsupported_batch.run.manifest_sha256.to_vec())
    .fetch_one(&pool)
    .await
    .map_err(|_| "no-write validation checkpoint side effects queried")?;
    ensure(
        validation_side_effects == 0,
        "valid and refused validation passes create no migration checkpoints",
    )?;
    litecr_application::migration::validate_batch(&first_batch)
        .map_err(|error| format!("synthetic migration batch validates: {error:?}"))?;
    let first = executor
        .commit_batch_atomically(&access, &first_batch)
        .await
        .map_err(|error| format!("supported synthetic source row should commit: {error:?}"))?;
    ensure(first.applied.len() == 1, "one source row is journaled")?;
    ensure(
        first.applied[0].public_id == public_id,
        "public ID is preserved",
    )?;

    let source_id = first.applied[0].source_record_id.clone();

    // An ordinary Registry version written after an import is outside that
    // manifest's journal. Reversing the older manifest must refuse the whole
    // operation and preserve both the later Registry edit and all durable
    // evidence written before the refusal.
    let protected_reverse_row = supported_row(
        source_system,
        &format!("later-edit-source-{key}"),
        &format!("later-edit-public-{key}"),
        &format!("later-edit-cruid-{key}"),
        &format!("later-edit-alias-{key}"),
        &format!("later-edit-mrn-{key}"),
    );
    let protected_reverse_batch = batch(
        run(
            &format!("later-edit-manifest-{key}"),
            &registry_a,
            &organization_a,
            1,
        ),
        1,
        vec![record(protected_reverse_row)],
    );
    let protected_reverse_import = executor
        .commit_batch_atomically(&access, &protected_reverse_batch)
        .await
        .map_err(|error| format!("source for later-edit reverse test imports: {error:?}"))?;
    let protected_source_id = protected_reverse_import.applied[0].source_record_id.clone();
    let protected_public_id = protected_reverse_import.applied[0].public_id.clone();
    let registry = RegistryService::new(Box::new(PostgresRegistryStore::new(pool.clone())), 100)
        .ok_or("ordinary Registry service accepts the configured page limit")?;
    let later_edit = registry
        .ingest(
            Some(&access),
            &format!("later-edit-idempotency-{key}"),
            SourceRecordCommand {
                registry_id: registry_a.clone(),
                organization_id: organization_a.clone(),
                source_system_id: source_system.to_owned(),
                source_record_key: format!("later-edit-source-{key}"),
                source_version: Some("ordinary-post-import-v2".to_owned()),
                assertions: vec![
                    SourceAssertion {
                        field_path: "name.original".to_owned(),
                        original_value: "ຄຳ ແກ້ໄຂຫຼັງການນຳເຂົ້າ".to_owned(),
                        language_tag: Some("lo".to_owned()),
                        script: Some("Lao".to_owned()),
                    },
                    SourceAssertion {
                        field_path: "birthDate".to_owned(),
                        original_value: "1984-07".to_owned(),
                        language_tag: None,
                        script: None,
                    },
                ],
                identifiers: Vec::new(),
            },
        )
        .await
        .map_err(|error| format!("ordinary post-import Registry edit commits: {error:?}"))?;
    ensure(
        later_edit.source_record_id == protected_source_id
            && later_edit.person_public_id == protected_public_id
            && later_edit.source_receive_version == 2
            && !later_edit.replayed,
        "ordinary Registry edit appends version 2 to the imported source outside its manifest",
    )?;
    let reverse_request = ReverseManifest {
        manifest_sha256: protected_reverse_batch.run.manifest_sha256,
        registry_id: registry_a.clone(),
        organization_id: organization_a.clone(),
        reason: "synthetic AC-22 guard against a later Registry edit".to_owned(),
    };
    let before_refused_reverse = reverse_guard_snapshot(
        &pool,
        &reverse_request.manifest_sha256,
        &protected_source_id,
    )
    .await?;
    let preflight = migration_service
        .reverse_preflight(
            Some(&access),
            &ReversePreflightRequest {
                manifest_sha256: reverse_request.manifest_sha256,
                registry_id: reverse_request.registry_id.clone(),
                organization_id: reverse_request.organization_id.clone(),
            },
        )
        .await
        .map_err(|error| format!("later-edit reverse preflight returns a decision: {error:?}"))?;
    ensure(
        !preflight.eligible
            && preflight
                .blockers
                .iter()
                .any(|blocker| blocker.code.as_str() == "later_source_version"),
        "preflight refuses reversal and reports the later source version",
    )?;
    let after_preflight = reverse_guard_snapshot(
        &pool,
        &reverse_request.manifest_sha256,
        &protected_source_id,
    )
    .await?;
    ensure(
        after_preflight == before_refused_reverse,
        "read-only preflight leaves source, manifest journal, audit, outbox, and receipt unchanged",
    )?;
    ensure(
        matches!(
            executor
                .reverse_manifest_atomically(&access, &reverse_request)
                .await,
            Err(MigrationError::ReverseBlocked)
        ),
        "transactional reverse recheck refuses the later source version",
    )?;
    let after_refused_reverse = reverse_guard_snapshot(
        &pool,
        &reverse_request.manifest_sha256,
        &protected_source_id,
    )
    .await?;
    ensure(
        after_refused_reverse == before_refused_reverse,
        "refused transactional reverse leaves source, manifest journal, audit, outbox, and receipt unchanged",
    )?;
    let latest_name: String = sqlx::query_scalar(
        "SELECT original_value FROM litecr.source_assertion WHERE source_record_id=$1::uuid AND receive_version=2 AND field_path='name.original' AND withdrawn_at IS NULL",
    )
    .bind(&protected_source_id)
    .fetch_one(&pool)
    .await
    .map_err(|_| "later source edit remains queryable after refused reverse")?;
    ensure(
        latest_name == "ຄຳ ແກ້ໄຂຫຼັງການນຳເຂົ້າ",
        "post-import Registry edit remains active after both reverse checks",
    )?;

    let current_receive_version: i64 = sqlx::query_scalar(
        "SELECT current_receive_version FROM litecr.source_record WHERE source_record_id=$1::uuid",
    )
    .bind(&source_id)
    .fetch_one(&pool)
    .await
    .map_err(|_| "current source receive version queried")?;
    ensure(
        current_receive_version == first.applied[0].receive_version,
        "accepted import advances the current source receive version",
    )?;
    let person_id: String = sqlx::query_scalar(
        "SELECT person_id::text FROM litecr.source_record WHERE source_record_id=$1::uuid",
    )
    .bind(&source_id)
    .fetch_one(&pool)
    .await
    .map_err(|_| "imported source record resolves to a person")?;
    let name = sqlx::query("SELECT original_value,language_tag,script FROM litecr.source_assertion WHERE source_record_id=$1::uuid AND field_path='name.original'")
        .bind(&source_id)
        .fetch_one(&pool)
        .await
        .map_err(|_| "original-script name assertion exists")?;
    ensure(
        name.try_get::<String, _>("original_value").ok().as_deref() == Some("ຄຳ ທົດສອບ")
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
        "original Lao value, language, and script are preserved",
    )?;
    let aliases: Vec<String> = sqlx::query_scalar(
        "SELECT alias_value FROM litecr.person_alias WHERE person_id=$1::uuid ORDER BY alias_value",
    )
    .bind(&person_id)
    .fetch_all(&pool)
    .await
    .map_err(|_| "person aliases queried")?;
    for expected in [&public_id, &cruid, &alias] {
        ensure(
            aliases.contains(expected),
            "public ID, CRUID, and legacy alias resolve",
        )?;
    }
    let identifier_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM litecr.person_identifier WHERE person_id=$1::uuid AND authority='urn:litecr:test:clinic' AND identifier_type='MRN' AND identifier_value=$2 AND authoritative",
    )
    .bind(&person_id)
    .bind(&identifier_value)
    .fetch_one(&pool)
    .await
    .map_err(|_| "authoritative identifier queried")?;
    ensure(
        identifier_count == 1,
        "authoritative identifier is attached once",
    )?;
    let journal_raw: Vec<u8> = sqlx::query_scalar(
        "SELECT original_payload_ndjson FROM litecr.migration_journal WHERE manifest_sha256=$1",
    )
    .bind(first_batch.run.manifest_sha256.to_vec())
    .fetch_one(&pool)
    .await
    .map_err(|_| "complete original row is present in migration journal")?;
    let journal_value: Value = serde_json::from_slice(&journal_raw)
        .map_err(|_| "journal retains valid original source NDJSON")?;
    ensure(
        journal_value["assertions"][1]["original_value"] == "1984-06"
            && journal_value["assertions"][1]["partial_date_precision"] == "month",
        "partial date string and precision are retained in the exact row journal",
    )?;

    let stored_checkpoint = PostgresMigrationJournal::new(pool.clone())
        .checkpoint(&first_batch.run)
        .await
        .map_err(|error| format!("committed migration checkpoint can be decoded: {error:?}"))?;
    ensure(
        stored_checkpoint.as_ref().is_some_and(|checkpoint| {
            checkpoint.committed_batch_sequence == 1 && checkpoint.committed_record_count == 1
        }),
        "completed migration checkpoint is available for retry",
    )?;
    let retried = executor
        .commit_batch_atomically(&access, &first_batch)
        .await
        .map_err(|error| {
            format!("same committed batch should replay its journal receipt: {error:?}")
        })?;
    ensure(
        retried == first,
        &format!(
            "same batch retry returns the original receipt; first={first:#?}; retried={retried:#?}"
        ),
    )?;
    for (query, label, expected) in [
        (
            "SELECT COUNT(*) FROM litecr.source_record_version WHERE source_record_id=$1::uuid",
            "source versions",
            1,
        ),
        (
            "SELECT COUNT(*) FROM litecr.source_assertion WHERE source_record_id=$1::uuid",
            "source assertions",
            2,
        ),
        (
            "SELECT COUNT(*) FROM litecr.migration_journal WHERE imported_source_record_id=$1::uuid",
            "journal rows",
            1,
        ),
        (
            "SELECT COUNT(*) FROM litecr.registry_idempotency WHERE source_record_id=$1::uuid",
            "idempotency rows",
            1,
        ),
        (
            "SELECT COUNT(*) FROM litecr.mutation_audit WHERE resource_ref=$1",
            "mutation audit rows",
            1,
        ),
        (
            "SELECT COUNT(*) FROM litecr.outbox_event WHERE aggregate_id=$1::uuid",
            "outbox rows",
            1,
        ),
    ] {
        let count: i64 = sqlx::query_scalar(query)
            .bind(&source_id)
            .fetch_one(&pool)
            .await
            .map_err(|_| format!("{label} queried after retry"))?;
        ensure(
            count == expected,
            &format!("retry does not duplicate {label}"),
        )?;
    }

    // Same source key/version, changed only in an alias (omitted from Registry
    // command_hash): the migration journal's canonical full-row digest must stop it.
    let mut changed_row = source_row;
    changed_row["aliases"] = json!([format!("changed-alias-{key}")]);
    let changed_batch = batch(
        run(&format!("changed-{key}"), &registry_a, &organization_a, 1),
        1,
        vec![record(changed_row)],
    );
    ensure(
        matches!(
            executor
                .commit_batch_atomically(&access, &changed_batch)
                .await,
            Err(MigrationError::CheckpointConflict)
        ),
        "changed canonical full-row digest across manifests fails closed",
    )?;
    let changed_checkpoint_exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM litecr.migration_checkpoint WHERE manifest_sha256=$1)",
    )
    .bind(changed_batch.run.manifest_sha256.to_vec())
    .fetch_one(&pool)
    .await
    .map_err(|_| "cross-manifest refusal checkpoint queried")?;
    ensure(
        !changed_checkpoint_exists,
        "refused run leaves no checkpoint",
    )?;
    ensure(
        matches!(
            executor
                .reverse_manifest_atomically(
                    &access,
                    &ReverseManifest {
                        manifest_sha256: changed_batch.run.manifest_sha256,
                        registry_id: registry_a.clone(),
                        organization_id: organization_a.clone(),
                        reason: "synthetic missing checkpoint reverse".into(),
                    },
                )
                .await,
            Err(MigrationError::CheckpointConflict)
        ),
        "reverse refuses a manifest with no checkpoint",
    )?;
    let empty_run = run(
        &format!("empty-import-{key}"),
        &registry_a,
        &organization_a,
        0,
    );
    PostgresMigrationJournal::new(pool.clone())
        .establish(&empty_run, &subject)
        .await
        .map_err(|error| format!("empty synthetic checkpoint establishes: {error:?}"))?;
    ensure(
        matches!(
            executor
                .reverse_manifest_atomically(
                    &access,
                    &ReverseManifest {
                        manifest_sha256: empty_run.manifest_sha256,
                        registry_id: registry_a.clone(),
                        organization_id: organization_a.clone(),
                        reason: "synthetic empty manifest reverse".into(),
                    },
                )
                .await,
            Err(MigrationError::CheckpointConflict)
        ),
        "reverse refuses an empty manifest checkpoint",
    )?;

    // Source keys and source versions are scoped to a registry and
    // organization. Reusing the same tuple in another authorized scope must
    // not be rejected by another tenant's migration journal.
    let cross_scope_row = supported_row(
        source_system,
        &source_key,
        &format!("scope-public-{key}"),
        &format!("scope-cruid-{key}"),
        &format!("scope-alias-{key}"),
        &format!("scope-mrn-{key}"),
    );
    let cross_scope_batch = batch(
        run(
            &format!("cross-scope-{key}"),
            &registry_b,
            &organization_b,
            1,
        ),
        1,
        vec![record(cross_scope_row)],
    );
    let cross_scope_commit = executor
        .commit_batch_atomically(&access, &cross_scope_batch)
        .await
        .map_err(|error| format!("same source tuple imports in a separate scope: {error:?}"))?;
    ensure(
        cross_scope_commit.applied.len() == 1
            && cross_scope_commit.applied[0].source_record_id != source_id,
        "same source tuple in another scope creates an independent source record",
    )?;

    // The first row is writeable, while the later row collides with the existing
    // public ID in another scope. Both the first row and checkpoint must roll back.
    let rollback_key = format!("rollback-first-{key}");
    let collision_key = format!("rollback-collision-{key}");
    let rollback_source_system = "synthetic-source";
    let rollback_row = supported_row(
        rollback_source_system,
        &rollback_key,
        &format!("rollback-public-{key}"),
        &format!("rollback-cruid-{key}"),
        &format!("rollback-alias-{key}"),
        &format!("rollback-mrn-{key}"),
    );
    let collision_row = supported_row(
        rollback_source_system,
        &collision_key,
        &public_id,
        &format!("collision-cruid-{key}"),
        &format!("collision-alias-{key}"),
        &format!("collision-mrn-{key}"),
    );
    let rollback_batch = batch(
        run(&format!("rollback-{key}"), &registry_b, &organization_b, 2),
        1,
        vec![record(rollback_row), record(collision_row)],
    );
    ensure(
        matches!(
            executor
                .commit_batch_atomically(&access, &rollback_batch)
                .await,
            Err(MigrationError::IdentityCollision)
        ),
        "later public-ID collision rejects the complete batch",
    )?;
    let rollback_rows: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM litecr.source_record WHERE registry_id=$1 AND organization_id=$2 AND source_record_key=$3",
    )
    .bind(&registry_b)
    .bind(&organization_b)
    .bind(&rollback_key)
    .fetch_one(&pool)
    .await
    .map_err(|_| "rollback source row queried")?;
    let rollback_checkpoint: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM litecr.migration_checkpoint WHERE manifest_sha256=$1)",
    )
    .bind(rollback_batch.run.manifest_sha256.to_vec())
    .fetch_one(&pool)
    .await
    .map_err(|_| "rollback checkpoint queried")?;
    ensure(
        rollback_rows == 0 && !rollback_checkpoint,
        "earlier source writes and checkpoint roll back atomically",
    )?;

    // A manifest can introduce a source and then import another version of
    // that same source in a later batch. Reversing it must withdraw the whole
    // source, rather than restore a predecessor version that is also withdrawn.
    let multi_source_key = format!("multi-version-source-{key}");
    let multi_public_id = format!("multi-version-public-{key}");
    let multi_cruid = format!("multi-version-cruid-{key}");
    let multi_alias = format!("multi-version-alias-{key}");
    let multi_identifier = format!("multi-version-mrn-{key}");
    let mut multi_v1 = supported_row(
        source_system,
        &multi_source_key,
        &multi_public_id,
        &multi_cruid,
        &multi_alias,
        &multi_identifier,
    );
    multi_v1["source_version"] = json!("multi-v1");
    let mut multi_v2 = multi_v1.clone();
    multi_v2["source_version"] = json!("multi-v2");
    multi_v2["assertions"][0]["original_value"] = json!("ຄຳ ທົດສອບ ຮຸ່ນສອງ");
    multi_v2["aliases"] = json!([multi_alias, format!("multi-version-added-alias-{key}")]);
    multi_v2["identifiers"].as_array_mut().unwrap().push(json!({
        "authority":"urn:litecr:test:clinic",
        "type":"MRN",
        "value":format!("multi-version-added-mrn-{key}"),
        "authoritative":true
    }));
    let multi_run = run(
        &format!("multi-version-{key}"),
        &registry_a,
        &organization_a,
        2,
    );
    let multi_first = batch(multi_run.clone(), 1, vec![record(multi_v1)]);
    let multi_first_commit = executor
        .commit_batch_atomically(&access, &multi_first)
        .await
        .map_err(|error| format!("first synthetic source version commits: {error:?}"))?;
    let multi_source_id = multi_first_commit.applied[0].source_record_id.clone();
    let multi_second = batch(multi_run, 2, vec![record(multi_v2)]);
    let multi_second_commit = executor
        .commit_batch_atomically(&access, &multi_second)
        .await
        .map_err(|error| format!("second synthetic source version commits: {error:?}"))?;
    ensure(
        multi_second_commit.applied[0].introduced_alias_ids.len() == 1
            && multi_second_commit.applied[0]
                .introduced_identifier_ids
                .len()
                == 1,
        "later batch introduces one additional alias and identifier effect",
    )?;
    let replayed_first_batch = executor
        .commit_batch_atomically(&access, &multi_first)
        .await
        .map_err(|error| format!("earlier committed batch replays after later batch: {error:?}"))?;
    ensure(
        replayed_first_batch.applied == multi_first_commit.applied,
        "earlier batch retry returns only its original effect IDs",
    )?;
    let multi_reverse = executor
        .reverse_manifest_atomically(
            &access,
            &ReverseManifest {
                manifest_sha256: multi_first.run.manifest_sha256,
                registry_id: registry_a.clone(),
                organization_id: organization_a.clone(),
                reason: "synthetic multi-version withdrawal".into(),
            },
        )
        .await
        .map_err(|error| format!("multi-version source reverses: {error:?}"))?;
    let multi_state = sqlx::query("SELECT current_receive_version,withdrawn_at IS NOT NULL AS withdrawn FROM litecr.source_record WHERE source_record_id=$1::uuid")
        .bind(&multi_source_id)
        .fetch_one(&pool)
        .await
        .map_err(|_| "multi-version source state queried")?;
    let multi_versions_withdrawn: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM litecr.source_record_version WHERE source_record_id=$1::uuid AND withdrawn_by_manifest=$2",
    )
    .bind(&multi_source_id)
    .bind(multi_first.run.manifest_sha256.to_vec())
    .fetch_one(&pool)
    .await
    .map_err(|_| "multi-version withdrawal queried")?;
    ensure(
        multi_reverse.withdrawn_record_count == 1
            && multi_reverse.detached_identifier_count == 2
            && multi_reverse.retained_alias_count == 4
            && multi_state.try_get::<bool, _>("withdrawn").ok() == Some(true)
            && multi_state
                .try_get::<i64, _>("current_receive_version")
                .ok()
                == Some(2)
            && multi_versions_withdrawn == 2,
        "manifest-created source stays withdrawn when every imported version is withdrawn",
    )?;

    // An operator may safely withdraw the committed subset when a later batch
    // failed. The uncommitted batch must never be synthesized or journaled.
    let partial_run = run(
        &format!("partial-import-{key}"),
        &registry_a,
        &organization_a,
        2,
    );
    let partial_source_key = format!("partial-import-committed-{key}");
    let partial_public_id = format!("partial-import-public-{key}");
    let partial_first_row = supported_row(
        source_system,
        &partial_source_key,
        &partial_public_id,
        &format!("partial-import-cruid-{key}"),
        &format!("partial-import-alias-{key}"),
        &format!("partial-import-mrn-{key}"),
    );
    let partial_first = batch(partial_run.clone(), 1, vec![record(partial_first_row)]);
    let partial_commit = executor
        .commit_batch_atomically(&access, &partial_first)
        .await
        .map_err(|error| format!("first partial-import batch commits: {error:?}"))?;
    let partial_expected_count: i64 = sqlx::query_scalar(
        "SELECT expected_record_count FROM litecr.migration_checkpoint WHERE manifest_sha256=$1",
    )
    .bind(partial_run.manifest_sha256.to_vec())
    .fetch_one(&pool)
    .await
    .map_err(|_| "partial manifest expected record count queried")?;
    ensure(
        partial_commit.checkpoint.committed_record_count == 1
            && partial_expected_count == 2
            && partial_commit.checkpoint.state
                == litecr_application::migration::MigrationRunState::Importing,
        "one committed batch leaves the manifest in importing state",
    )?;
    let partial_source_id = partial_commit.applied[0].source_record_id.clone();
    let partial_second_key = format!("partial-import-uncommitted-{key}");
    let partial_second_row = supported_row(
        source_system,
        &partial_second_key,
        &format!("partial-import-uncommitted-public-{key}"),
        &format!("partial-import-uncommitted-cruid-{key}"),
        &format!("partial-import-uncommitted-alias-{key}"),
        &format!("partial-import-uncommitted-mrn-{key}"),
    );
    let partial_second = batch(partial_run.clone(), 2, vec![record(partial_second_row)]);
    let partial_reverse_request = ReverseManifest {
        manifest_sha256: partial_run.manifest_sha256,
        registry_id: registry_a.clone(),
        organization_id: organization_a.clone(),
        reason: "synthetic partial import failure rollback".into(),
    };
    let partial_reverse = executor
        .reverse_manifest_atomically(&access, &partial_reverse_request)
        .await
        .map_err(|error| format!("committed subset of partial import reverses: {error:?}"))?;
    let partial_retry = executor
        .reverse_manifest_atomically(&access, &partial_reverse_request)
        .await
        .map_err(|error| format!("partial reverse retry returns its receipt: {error:?}"))?;
    let partial_state: String = sqlx::query_scalar(
        "SELECT state FROM litecr.migration_checkpoint WHERE manifest_sha256=$1",
    )
    .bind(partial_run.manifest_sha256.to_vec())
    .fetch_one(&pool)
    .await
    .map_err(|_| "partial checkpoint state queried")?;
    let partial_journal_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM litecr.migration_journal WHERE manifest_sha256=$1",
    )
    .bind(partial_run.manifest_sha256.to_vec())
    .fetch_one(&pool)
    .await
    .map_err(|_| "partial manifest journal count queried")?;
    let partial_source_withdrawn: bool = sqlx::query_scalar(
        "SELECT withdrawn_at IS NOT NULL FROM litecr.source_record WHERE source_record_id=$1::uuid",
    )
    .bind(&partial_source_id)
    .fetch_one(&pool)
    .await
    .map_err(|_| "committed partial source withdrawal queried")?;
    let uncommitted_source_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM litecr.source_record WHERE registry_id=$1 AND organization_id=$2 AND source_record_key=$3",
    )
    .bind(&registry_a)
    .bind(&organization_a)
    .bind(&partial_second_key)
    .fetch_one(&pool)
    .await
    .map_err(|_| "uncommitted partial source absence queried")?;
    ensure(
        partial_reverse == partial_retry
            && partial_reverse.withdrawn_record_count == 1
            && partial_state == "reversed"
            && partial_journal_count == 1
            && partial_source_withdrawn
            && uncommitted_source_count == 0,
        "partial reverse is idempotent and withdraws only its one committed journaled row",
    )?;
    ensure(
        matches!(
            executor
                .commit_batch_atomically(&access, &partial_second)
                .await,
            Err(MigrationError::CheckpointConflict)
        ),
        "a later batch cannot apply after the partial manifest is reversed",
    )?;

    // A source that existed before this manifest must remain active and point
    // back to its exact pre-import accepted version after safe withdrawal.
    let prior_source_key = format!("preexisting-source-{key}");
    let prior_public_id = format!("preexisting-public-{key}");
    let prior_cruid = format!("preexisting-cruid-{key}");
    let prior_alias = format!("preexisting-alias-{key}");
    let prior_identifier = format!("preexisting-mrn-{key}");
    let mut prior_v1 = supported_row(
        source_system,
        &prior_source_key,
        &prior_public_id,
        &prior_cruid,
        &prior_alias,
        &prior_identifier,
    );
    prior_v1["source_version"] = json!("preexisting-v1");
    let prior_run = run(
        &format!("preexisting-base-{key}"),
        &registry_a,
        &organization_a,
        1,
    );
    let prior_batch = batch(prior_run, 1, vec![record(prior_v1.clone())]);
    let prior_commit = executor
        .commit_batch_atomically(&access, &prior_batch)
        .await
        .map_err(|error| format!("pre-existing source baseline imports: {error:?}"))?;
    let prior_source_id = prior_commit.applied[0].source_record_id.clone();
    let prior_person_id: String = sqlx::query_scalar(
        "SELECT person_id::text FROM litecr.source_record WHERE source_record_id=$1::uuid",
    )
    .bind(&prior_source_id)
    .fetch_one(&pool)
    .await
    .map_err(|_| "pre-existing source person resolved")?;
    let prior_resource_version: i64 =
        sqlx::query_scalar("SELECT resource_version FROM litecr.person WHERE person_id=$1::uuid")
            .bind(&prior_person_id)
            .fetch_one(&pool)
            .await
            .map_err(|_| "pre-existing person resource version queried")?;
    let mut imported_v2 = prior_v1;
    imported_v2["source_version"] = json!("preexisting-v2");
    imported_v2["assertions"][0]["original_value"] = json!("ຄຳ ຮຸ່ນໃໝ່");
    let update_run = run(
        &format!("preexisting-update-{key}"),
        &registry_a,
        &organization_a,
        1,
    );
    let update_batch = batch(update_run, 1, vec![record(imported_v2)]);
    let update_commit = executor
        .commit_batch_atomically(&access, &update_batch)
        .await
        .map_err(|error| format!("pre-existing source receives migration update: {error:?}"))?;
    let imported_resource_version: i64 =
        sqlx::query_scalar("SELECT resource_version FROM litecr.person WHERE person_id=$1::uuid")
            .bind(&prior_person_id)
            .fetch_one(&pool)
            .await
            .map_err(|_| "person resource version queried after imported update")?;
    ensure(
        update_commit.applied[0].source_record_id == prior_source_id
            && !update_commit.applied[0].source_record_created
            && update_commit.applied[0].previous_receive_version == Some(1)
            && imported_resource_version == prior_resource_version + 1,
        "migration update journals the existing source and its baseline version",
    )?;
    let update_reverse = executor
        .reverse_manifest_atomically(
            &access,
            &ReverseManifest {
                manifest_sha256: update_batch.run.manifest_sha256,
                registry_id: registry_a.clone(),
                organization_id: organization_a.clone(),
                reason: "synthetic pre-existing source rollback".into(),
            },
        )
        .await
        .map_err(|error| format!("pre-existing source migration reverses: {error:?}"))?;
    let restored_source = sqlx::query("SELECT current_receive_version,withdrawn_at IS NOT NULL AS withdrawn FROM litecr.source_record WHERE source_record_id=$1::uuid")
        .bind(&prior_source_id)
        .fetch_one(&pool)
        .await
        .map_err(|_| "pre-existing source state queried after reverse")?;
    let restored_assertion: String = sqlx::query_scalar(
        "SELECT a.original_value FROM litecr.source_assertion a WHERE a.source_record_id=$1::uuid AND a.receive_version=1 AND a.field_path='name.original'",
    )
    .bind(&prior_source_id)
    .fetch_one(&pool)
    .await
    .map_err(|_| "pre-existing source assertion retained")?;
    let restored_person = PostgresRegistryStore::new(pool.clone())
        .person(&registry_a, &organization_a, &prior_public_id)
        .await
        .map_err(|error| format!("restored pre-existing person lookup succeeds: {error:?}"))?;
    let restored_identifier_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM litecr.person_identifier WHERE person_id=$1::uuid AND identifier_value=$2",
    )
    .bind(&prior_person_id)
    .bind(&prior_identifier)
    .fetch_one(&pool)
    .await
    .map_err(|_| "pre-existing identifier state queried")?;
    let reversed_resource_version: i64 =
        sqlx::query_scalar("SELECT resource_version FROM litecr.person WHERE person_id=$1::uuid")
            .bind(&prior_person_id)
            .fetch_one(&pool)
            .await
            .map_err(|_| "person resource version queried after reversal")?;
    ensure(
        update_reverse.withdrawn_record_count == 1
            && restored_source
                .try_get::<i64, _>("current_receive_version")
                .ok()
                == Some(1)
            && restored_source.try_get::<bool, _>("withdrawn").ok() == Some(false)
            && restored_assertion == "ຄຳ ທົດສອບ"
            && restored_person.is_some()
            && reversed_resource_version == imported_resource_version + 1
            && restored_person
                .as_ref()
                .is_some_and(|person| person.resource_version == reversed_resource_version)
            && restored_identifier_count == 1,
        "reversal restores the pre-import source value while advancing the active Patient resource version",
    )?;

    let reverse_request = ReverseManifest {
        manifest_sha256: first_batch.run.manifest_sha256,
        registry_id: registry_a.clone(),
        organization_id: organization_a.clone(),
        reason: "synthetic migration withdrawal test".into(),
    };
    let unauthorized_reverse =
        MigrationService::new(Box::new(PostgresMigrationExecutor::new(pool.clone())))
            .reverse_manifest(
                Some(&access),
                ReverseManifest {
                    manifest_sha256: reverse_request.manifest_sha256,
                    registry_id: format!("ungranted-registry-{key}"),
                    organization_id: format!("ungranted-organization-{key}"),
                    reason: "synthetic unauthorized reverse attempt".into(),
                },
            )
            .await;
    ensure(
        matches!(unauthorized_reverse, Err(MigrationError::AccessDenied(_))),
        "reverse is denied before executor access when the requested scope is unauthorized",
    )?;
    let preflight_request = ReversePreflightRequest {
        manifest_sha256: reverse_request.manifest_sha256,
        registry_id: registry_a.clone(),
        organization_id: organization_a.clone(),
    };
    let before_preflight_writes: (i64, i64, i64) = sqlx::query_as(
        "SELECT (SELECT COUNT(*) FROM litecr.migration_reverse_receipt WHERE manifest_sha256=$1), (SELECT COUNT(*) FROM litecr.mutation_audit WHERE action='migration.source.withdraw' AND resource_ref=$2), (SELECT COUNT(*) FROM litecr.outbox_event WHERE event_type='registry.source_record.withdrawn' AND aggregate_id=$2::uuid)",
    ).bind(reverse_request.manifest_sha256.to_vec()).bind(&source_id).fetch_one(&pool).await.map_err(|_| "reverse write counts read before preflight")?;
    let service = MigrationService::new(Box::new(PostgresMigrationExecutor::new(pool.clone())));
    let preflight = service
        .reverse_preflight(Some(&access), &preflight_request)
        .await
        .map_err(|error| format!("eligible reverse preflight completes: {error:?}"))?;
    let after_preflight_writes: (i64, i64, i64) = sqlx::query_as(
        "SELECT (SELECT COUNT(*) FROM litecr.migration_reverse_receipt WHERE manifest_sha256=$1), (SELECT COUNT(*) FROM litecr.mutation_audit WHERE action='migration.source.withdraw' AND resource_ref=$2), (SELECT COUNT(*) FROM litecr.outbox_event WHERE event_type='registry.source_record.withdrawn' AND aggregate_id=$2::uuid)",
    ).bind(reverse_request.manifest_sha256.to_vec()).bind(&source_id).fetch_one(&pool).await.map_err(|_| "reverse write counts read after preflight")?;
    ensure(
        preflight.eligible
            && preflight.blockers.is_empty()
            && before_preflight_writes == after_preflight_writes,
        "preflight reports aggregate eligibility without writing receipt, audit, or outbox rows",
    )?;
    let source_receive_version: i64 = sqlx::query_scalar(
        "SELECT current_receive_version FROM litecr.source_record WHERE source_record_id=$1::uuid",
    )
    .bind(&source_id)
    .fetch_one(&pool)
    .await
    .map_err(|_| "current source version read after preflight")?;
    let original_payload_hash: Vec<u8> = sqlx::query_scalar("SELECT expected_current_payload_sha256 FROM litecr.migration_journal WHERE manifest_sha256=$1 ORDER BY imported_receive_version DESC LIMIT 1")
        .bind(reverse_request.manifest_sha256.to_vec()).fetch_one(&pool).await.map_err(|_| "reverse journal payload digest read")?;
    sqlx::query("UPDATE litecr.source_record_version SET payload_hash=$3 WHERE source_record_id=$1::uuid AND receive_version=$2")
        .bind(&source_id).bind(source_receive_version).bind(vec![0_u8; 32]).execute(&pool).await.map_err(|_| "test mutates the source version after preview")?;
    let stale_reverse = executor
        .reverse_manifest_atomically(&access, &reverse_request)
        .await;
    ensure(
        matches!(stale_reverse, Err(MigrationError::ReverseBlocked)),
        "a stale eligible preview cannot bypass the atomic reverse recheck",
    )?;
    sqlx::query("UPDATE litecr.source_record_version SET payload_hash=$3 WHERE source_record_id=$1::uuid AND receive_version=$2")
        .bind(&source_id).bind(source_receive_version).bind(original_payload_hash).execute(&pool).await.map_err(|_| "test restores its synthetic source payload digest")?;
    let reverse = executor
        .reverse_manifest_atomically(&access, &reverse_request)
        .await
        .map_err(|error| format!("safe synthetic manifest withdrawal commits: {error:?}"))?;
    ensure(
        reverse.withdrawn_record_count == 1
            && reverse.detached_identifier_count == 1
            && reverse.retained_alias_count == 3,
        "reverse receipt counts withdrawn source, detached introduced identifier, and retained aliases",
    )?;
    let retry = executor
        .reverse_manifest_atomically(&access, &reverse_request)
        .await
        .map_err(|error| format!("same reverse request replays its receipt: {error:?}"))?;
    ensure(
        retry == reverse,
        "reverse retry returns the original receipt",
    )?;
    ensure(
        matches!(
            executor
                .commit_batch_atomically(&access, &first_batch)
                .await,
            Err(MigrationError::CheckpointConflict)
        ),
        "a reversed manifest cannot replay its committed import batch",
    )?;

    let source_state = sqlx::query("SELECT current_receive_version,withdrawn_at IS NOT NULL AS withdrawn FROM litecr.source_record WHERE source_record_id=$1::uuid")
        .bind(&source_id).fetch_one(&pool).await.map_err(|_| "withdrawn source state queried")?;
    ensure(
        source_state.try_get::<bool, _>("withdrawn").ok() == Some(true),
        "newly introduced source is soft-withdrawn, not deleted",
    )?;
    let versions_withdrawn: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM litecr.source_record_version WHERE source_record_id=$1::uuid AND withdrawn_by_manifest=$2",
    )
    .bind(&source_id)
    .bind(first_batch.run.manifest_sha256.to_vec())
    .fetch_one(&pool)
    .await
    .map_err(|_| "manifest-owned source version withdrawal queried")?;
    let assertions_withdrawn: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM litecr.source_assertion WHERE source_record_id=$1::uuid AND withdrawn_by_manifest=$2",
    )
    .bind(&source_id)
    .bind(first_batch.run.manifest_sha256.to_vec())
    .fetch_one(&pool)
    .await
    .map_err(|_| "manifest-owned assertions withdrawal queried")?;
    ensure(
        versions_withdrawn == 1 && assertions_withdrawn == 2,
        "only the imported source version and its assertions are marked withdrawn",
    )?;
    let retained_aliases: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM litecr.person_alias WHERE person_id=$1::uuid")
            .bind(&person_id)
            .fetch_one(&pool)
            .await
            .map_err(|_| "retained alias rows queried")?;
    let attached_identifiers: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM litecr.person_identifier WHERE source_record_id=$1::uuid",
    )
    .bind(&source_id)
    .fetch_one(&pool)
    .await
    .map_err(|_| "detached authoritative identifiers queried")?;
    ensure(
        retained_aliases == 3 && attached_identifiers == 0,
        "aliases remain because consumer usage is not persisted; unshared introduced identifier detaches",
    )?;

    let registry = PostgresRegistryStore::new(pool.clone());
    let visible_person = registry
        .person(&registry_a, &organization_a, &public_id)
        .await
        .map_err(|error| format!("withdrawn identity projection is queryable: {error:?}"))?;
    ensure(
        visible_person.is_none(),
        "registry person lookup excludes an identity with only withdrawn source evidence",
    )?;
    let source_person = registry
        .person_for_source(&registry_a, &organization_a, &source_id)
        .await
        .map_err(|error| format!("withdrawn source lookup completes: {error:?}"))?;
    ensure(
        source_person.is_none(),
        "source-reference lookup hides the withdrawn source",
    )?;
    let identifier_matches = registry
        .search_identifier(
            &registry_a,
            &organization_a,
            "urn:litecr:test:clinic",
            &identifier_value,
            10,
        )
        .await
        .map_err(|error| format!("withdrawn identifier search completes: {error:?}"))?;
    ensure(
        identifier_matches.is_empty(),
        "identifier search excludes a withdrawn-only assertion",
    )?;
    let matching = PostgresMatchingStore::new(pool.clone());
    ensure(
        matches!(
            matching
                .evaluate(
                    &registry_a,
                    &organization_a,
                    &source_id,
                    100,
                    &subject,
                    "migration-withdrawn-query",
                    None,
                )
                .await,
            Err(litecr_application::matching::MatchingStoreError::NotFound)
        ),
        "matching rejects a withdrawn query source",
    )?;
    let mapped = litecr_infrastructure::migration::map_supported_record(
        &first_batch.run,
        &first_batch.records[0],
    )
    .map_err(|error| format!("test row remaps for post-withdrawal write check: {error:?}"))?;
    let mut later_command = mapped.command;
    later_command.source_version = Some("must-not-revive".into());
    let source_writer = litecr_application::registry::RegistryActor {
        kind: ActorKind::Human,
        id: subject.clone(),
        issuer_url: Some(SYNTHETIC_ISSUER.to_owned()),
        request_id: "migration-withdrawn-write".into(),
        correlation_id: None,
    };
    ensure(
        registry
            .ingest(&source_writer, "withdrawn-source-new-key", &later_command)
            .await
            .is_err(),
        "ordinary source ingestion fails closed instead of reviving a withdrawn source",
    )?;

    let journal_state: String = sqlx::query_scalar(
        "SELECT reverse_state FROM litecr.migration_journal WHERE manifest_sha256=$1",
    )
    .bind(first_batch.run.manifest_sha256.to_vec())
    .fetch_one(&pool)
    .await
    .map_err(|_| "reverse journal state queried")?;
    let correction_events: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM litecr.outbox_event WHERE aggregate_id=$1::uuid AND event_type='registry.source_record.withdrawn'",
    )
    .bind(&source_id)
    .fetch_one(&pool)
    .await
    .map_err(|_| "correction outbox event queried")?;
    let correction_audits: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM litecr.mutation_audit WHERE resource_ref=$1 AND action='migration.source.withdraw'",
    )
    .bind(&source_id)
    .fetch_one(&pool)
    .await
    .map_err(|_| "correction audit queried")?;
    ensure(
        journal_state == "reversed" && correction_events == 1 && correction_audits == 1,
        "journal, correction audit, and outbox event commit exactly once",
    )?;

    // Keep the context actor identifier for exact cleanup of test-owned records.
    ensure(
        subject.ends_with(&key),
        "synthetic actor is uniquely scoped",
    )?;
    pool.close().await;
    Ok(())
}

async fn provisioned_access(
    pool: &PgPool,
    key: &str,
    registry_a: &str,
    organization_a: &str,
    registry_b: &str,
    organization_b: &str,
) -> Result<(litecr_application::access::AccessContext, String), String> {
    let subject = format!("migration-registrar-{key}");
    sqlx::query("INSERT INTO litecr.access_identity (actor_kind,issuer_url,external_subject) VALUES ('human',$1,$2)")
        .bind(SYNTHETIC_ISSUER)
        .bind(&subject)
        .execute(pool)
        .await
        .map_err(|_| "synthetic human identity inserted")?;
    for (registry_id, organization_id) in
        [(registry_a, organization_a), (registry_b, organization_b)]
    {
        sqlx::query("INSERT INTO litecr.access_grant (actor_kind,issuer_url,external_subject,registry_id,organization_id,record_ids,role_name,permission_name) VALUES ('human',$1,$2,$3,$4,NULL,'registrar',NULL)")
            .bind(SYNTHETIC_ISSUER)
            .bind(&subject)
            .bind(registry_id)
            .bind(organization_id)
            .execute(pool)
            .await
            .map_err(|_| "synthetic scoped registrar grant inserted")?;
        sqlx::query("INSERT INTO litecr.access_grant (actor_kind,issuer_url,external_subject,registry_id,organization_id,record_ids,role_name,permission_name) VALUES ('human',$1,$2,$3,$4,NULL,NULL,'migration:write')")
            .bind(SYNTHETIC_ISSUER)
            .bind(&subject)
            .bind(registry_id)
            .bind(organization_id)
            .execute(pool)
            .await
            .map_err(|_| "synthetic scoped migration permission inserted")?;
    }
    let directory = PostgresAccessDirectory::new(pool.clone());
    let sessions = PostgresAccessSessionRepository::new(pool.clone(), directory.clone());
    let actor = ActorRef {
        kind: ActorKind::Human,
        id: subject.clone(),
        issuer_url: Some(SYNTHETIC_ISSUER.to_owned()),
    };
    let session = sessions
        .create(
            &actor,
            Some("synthetic-migration-session"),
            600,
            "migration-session-create",
        )
        .await
        .map_err(|_| "preprovisioned synthetic registrar session created")?;
    let service = AccessService::new(
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
    .ok_or("synthetic AccessService configuration is valid")?;
    let context = service
        .authenticate(
            &session.opaque_token,
            Some(&session.csrf_token),
            true,
            "migration-authenticate",
        )
        .await
        .map_err(|_| "existing authenticator resolves authorized registrar session")?;
    Ok((context, subject))
}

struct UnusedOidcProvider;

#[async_trait]
impl OidcProvider for UnusedOidcProvider {
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

async fn cleanup(database_url: &str, key: &str) {
    let pool = connect(database_url, 4)
        .await
        .expect("isolated synthetic DB remains available for cleanup");
    let registry_a = format!("migration-reg-a-{key}");
    let organization_a = format!("migration-org-a-{key}");
    let registry_b = format!("migration-reg-b-{key}");
    let organization_b = format!("migration-org-b-{key}");
    let subject = format!("migration-registrar-{key}");
    sqlx::query("DELETE FROM litecr.migration_package_v2_batch WHERE upload_id IN (SELECT upload_id FROM litecr.migration_package_v2 WHERE (registry_id=$1 AND organization_id=$2) OR (registry_id=$3 AND organization_id=$4))")
        .bind(&registry_a)
        .bind(&organization_a)
        .bind(&registry_b)
        .bind(&organization_b)
        .execute(&pool)
        .await
        .expect("test-owned v2 raw staging batches removed first");
    sqlx::query("DELETE FROM litecr.migration_package_v2 WHERE (registry_id=$1 AND organization_id=$2) OR (registry_id=$3 AND organization_id=$4)")
        .bind(&registry_a)
        .bind(&organization_a)
        .bind(&registry_b)
        .bind(&organization_b)
        .execute(&pool)
        .await
        .expect("test-owned v2 package metadata removed");
    let source_ids: Vec<String> = sqlx::query_scalar(
        "SELECT source_record_id::text FROM litecr.source_record WHERE (registry_id=$1 AND organization_id=$2) OR (registry_id=$3 AND organization_id=$4)",
    )
    .bind(&registry_a)
    .bind(&organization_a)
    .bind(&registry_b)
    .bind(&organization_b)
    .fetch_all(&pool)
    .await
    .expect("only unique synthetic scope source rows selected");
    sqlx::query("DELETE FROM litecr.migration_quarantine_disposition_item WHERE disposition_id IN (SELECT disposition_id FROM litecr.migration_quarantine_disposition WHERE (registry_id=$1 AND organization_id=$2) OR (registry_id=$3 AND organization_id=$4))")
        .bind(&registry_a)
        .bind(&organization_a)
        .bind(&registry_b)
        .bind(&organization_b)
        .execute(&pool)
        .await
        .expect("test-owned disposition evidence removed");
    sqlx::query("DELETE FROM litecr.migration_quarantine_disposition WHERE (registry_id=$1 AND organization_id=$2) OR (registry_id=$3 AND organization_id=$4)")
        .bind(&registry_a)
        .bind(&organization_a)
        .bind(&registry_b)
        .bind(&organization_b)
        .execute(&pool)
        .await
        .expect("test-owned disposition records removed");
    sqlx::query("DELETE FROM litecr.migration_reverse_receipt WHERE manifest_sha256 IN (SELECT manifest_sha256 FROM litecr.migration_checkpoint WHERE (registry_id=$1 AND organization_id=$2) OR (registry_id=$3 AND organization_id=$4))")
        .bind(&registry_a)
        .bind(&organization_a)
        .bind(&registry_b)
        .bind(&organization_b)
        .execute(&pool)
        .await
        .expect("test-owned reverse receipts removed first");
    sqlx::query("DELETE FROM litecr.migration_journal WHERE manifest_sha256 IN (SELECT manifest_sha256 FROM litecr.migration_checkpoint WHERE (registry_id=$1 AND organization_id=$2) OR (registry_id=$3 AND organization_id=$4))")
        .bind(&registry_a)
        .bind(&organization_a)
        .bind(&registry_b)
        .bind(&organization_b)
        .execute(&pool)
        .await
        .expect("test-owned migration journal rows removed first");
    sqlx::query("DELETE FROM litecr.migration_effect WHERE manifest_sha256 IN (SELECT manifest_sha256 FROM litecr.migration_checkpoint WHERE (registry_id=$1 AND organization_id=$2) OR (registry_id=$3 AND organization_id=$4))")
        .bind(&registry_a)
        .bind(&organization_a)
        .bind(&registry_b)
        .bind(&organization_b)
        .execute(&pool)
        .await
        .expect("test-owned migration effects removed");
    sqlx::query("DELETE FROM litecr.migration_batch WHERE manifest_sha256 IN (SELECT manifest_sha256 FROM litecr.migration_checkpoint WHERE (registry_id=$1 AND organization_id=$2) OR (registry_id=$3 AND organization_id=$4))")
        .bind(&registry_a)
        .bind(&organization_a)
        .bind(&registry_b)
        .bind(&organization_b)
        .execute(&pool)
        .await
        .expect("test-owned batches removed");
    sqlx::query("DELETE FROM litecr.migration_checkpoint WHERE (registry_id=$1 AND organization_id=$2) OR (registry_id=$3 AND organization_id=$4)")
        .bind(&registry_a)
        .bind(&organization_a)
        .bind(&registry_b)
        .bind(&organization_b)
        .execute(&pool)
        .await
        .expect("test-owned checkpoints removed");
    sqlx::query(
        "DELETE FROM litecr.registry_idempotency WHERE actor_id=$1 AND registry_id IN ($2,$3)",
    )
    .bind(&subject)
    .bind(&registry_a)
    .bind(&registry_b)
    .execute(&pool)
    .await
    .expect("test-owned idempotency rows removed");
    sqlx::query(
        "DELETE FROM litecr.consumer_receipt r USING litecr.outbox_event o \
         WHERE r.event_id=o.event_id AND o.actor_id=$1 AND \
           ((o.registry_id=$2 AND o.organization_id=$3) OR (o.registry_id=$4 AND o.organization_id=$5))",
    )
    .bind(&subject)
    .bind(&registry_a)
    .bind(&organization_a)
    .bind(&registry_b)
    .bind(&organization_b)
    .execute(&pool)
    .await
    .expect("test-owned scoped consumer receipts removed before events");
    sqlx::query(
        "DELETE FROM litecr.consumer_job j USING litecr.outbox_event o \
         WHERE j.event_id=o.event_id AND o.actor_id=$1 AND \
           ((o.registry_id=$2 AND o.organization_id=$3) OR (o.registry_id=$4 AND o.organization_id=$5))",
    )
    .bind(&subject)
    .bind(&registry_a)
    .bind(&organization_a)
    .bind(&registry_b)
    .bind(&organization_b)
    .execute(&pool)
    .await
    .expect("test-owned scoped consumer jobs removed before events");
    sqlx::query("DELETE FROM litecr.outbox_event WHERE actor_id=$1 AND ((registry_id=$2 AND organization_id=$3) OR (registry_id=$4 AND organization_id=$5))")
        .bind(&subject)
        .bind(&registry_a)
        .bind(&organization_a)
        .bind(&registry_b)
        .bind(&organization_b)
        .execute(&pool)
        .await
        .expect("test-owned outbox rows removed");
    sqlx::query("DELETE FROM litecr.mutation_audit WHERE actor_id=$1 AND ((registry_id=$2 AND organization_id=$3) OR (registry_id=$4 AND organization_id=$5))")
        .bind(&subject)
        .bind(&registry_a)
        .bind(&organization_a)
        .bind(&registry_b)
        .bind(&organization_b)
        .execute(&pool)
        .await
        .expect("test-owned mutation audit rows removed");
    if !source_ids.is_empty() {
        sqlx::query("DELETE FROM matching.normalized_assertion WHERE source_record_id::text=ANY($1::text[])")
            .bind(&source_ids)
            .execute(&pool)
            .await
            .expect("test-owned normalized assertion keys removed before their source assertions");
        sqlx::query("DELETE FROM matching.normalized_identifier WHERE source_record_id::text=ANY($1::text[])")
            .bind(&source_ids)
            .execute(&pool)
            .await
            .expect("test-owned normalized identifier keys removed before source versions");
        sqlx::query("DELETE FROM matching.indexed_source_version WHERE source_record_id::text=ANY($1::text[])")
            .bind(&source_ids)
            .execute(&pool)
            .await
            .expect("test-owned matching index markers removed before source versions");
        sqlx::query("DELETE FROM litecr.source_identifier_assertion WHERE source_record_id::text=ANY($1::text[])")
            .bind(&source_ids)
            .execute(&pool)
            .await
            .expect("test-owned source identifier assertions removed");
        sqlx::query(
            "DELETE FROM litecr.person_identifier WHERE source_record_id::text=ANY($1::text[])",
        )
        .bind(&source_ids)
        .execute(&pool)
        .await
        .expect("test-owned person identifiers removed");
        sqlx::query(
            "DELETE FROM litecr.source_assertion WHERE source_record_id::text=ANY($1::text[])",
        )
        .bind(&source_ids)
        .execute(&pool)
        .await
        .expect("test-owned source assertions removed");
        sqlx::query(
            "DELETE FROM litecr.source_record_version WHERE source_record_id::text=ANY($1::text[])",
        )
        .bind(&source_ids)
        .execute(&pool)
        .await
        .expect("test-owned source versions removed");
        sqlx::query(
            "DELETE FROM litecr.source_record WHERE source_record_id::text=ANY($1::text[])",
        )
        .bind(&source_ids)
        .execute(&pool)
        .await
        .expect("test-owned source records removed");
    }
    let public_ids = [
        format!("public-{key}"),
        format!("later-edit-public-{key}"),
        format!("scope-public-{key}"),
        format!("rollback-public-{key}"),
        format!("multi-version-public-{key}"),
        format!("preexisting-public-{key}"),
        format!("partial-import-public-{key}"),
    ];
    let person_ids: Vec<String> = sqlx::query_scalar(
        "SELECT person_id::text FROM litecr.person WHERE public_id=ANY($1::text[])",
    )
    .bind(&public_ids)
    .fetch_all(&pool)
    .await
    .expect("test-owned persons selected by exact public IDs");
    if !person_ids.is_empty() {
        sqlx::query("DELETE FROM litecr.person_alias WHERE person_id::text=ANY($1::text[])")
            .bind(&person_ids)
            .execute(&pool)
            .await
            .expect("test-owned aliases removed");
        sqlx::query("DELETE FROM litecr.person WHERE person_id::text=ANY($1::text[])")
            .bind(&person_ids)
            .execute(&pool)
            .await
            .expect("test-owned persons removed");
    }
    sqlx::query("DELETE FROM litecr.access_session WHERE actor_kind='human' AND issuer_url=$1 AND external_subject=$2")
        .bind(SYNTHETIC_ISSUER)
        .bind(&subject)
        .execute(&pool)
        .await
        .expect("test-owned session removed");
    sqlx::query("DELETE FROM litecr.access_grant WHERE actor_kind='human' AND issuer_url=$1 AND external_subject=$2")
        .bind(SYNTHETIC_ISSUER)
        .bind(&subject)
        .execute(&pool)
        .await
        .expect("test-owned grants removed");
    sqlx::query("DELETE FROM litecr.access_audit WHERE actor_issuer_url=$1 AND actor_id=$2")
        .bind(SYNTHETIC_ISSUER)
        .bind(&subject)
        .execute(&pool)
        .await
        .expect("test-owned access audit rows removed");
    sqlx::query("DELETE FROM litecr.access_identity WHERE actor_kind='human' AND issuer_url=$1 AND external_subject=$2")
        .bind(SYNTHETIC_ISSUER)
        .bind(&subject)
        .execute(&pool)
        .await
        .expect("test-owned identity removed last");
    pool.close().await;
}

async fn reverse_guard_snapshot(
    pool: &PgPool,
    manifest_sha256: &[u8; 32],
    source_record_id: &str,
) -> Result<Value, String> {
    sqlx::query_scalar(
        "SELECT jsonb_build_object(\
            'source', (SELECT to_jsonb(s) FROM litecr.source_record s WHERE s.source_record_id=$2::uuid), \
            'versions', COALESCE((SELECT jsonb_agg(to_jsonb(v) ORDER BY v.receive_version) FROM litecr.source_record_version v WHERE v.source_record_id=$2::uuid), '[]'::jsonb), \
            'assertions', COALESCE((SELECT jsonb_agg(to_jsonb(a) ORDER BY a.receive_version,a.field_path) FROM litecr.source_assertion a WHERE a.source_record_id=$2::uuid), '[]'::jsonb), \
            'identifier_assertions', COALESCE((SELECT jsonb_agg(to_jsonb(a) ORDER BY a.receive_version,a.authority,a.identifier_type,a.original_value) FROM litecr.source_identifier_assertion a WHERE a.source_record_id=$2::uuid), '[]'::jsonb), \
            'checkpoint', (SELECT to_jsonb(c) FROM litecr.migration_checkpoint c WHERE c.manifest_sha256=$1), \
            'journal', COALESCE((SELECT jsonb_agg(to_jsonb(j) ORDER BY j.imported_receive_version) FROM litecr.migration_journal j WHERE j.manifest_sha256=$1 AND j.imported_source_record_id=$2::uuid), '[]'::jsonb), \
            'effects', COALESCE((SELECT jsonb_agg(to_jsonb(e) ORDER BY e.effect_kind,e.effect_ref) FROM litecr.migration_effect e WHERE e.manifest_sha256=$1 AND e.source_record_id=$2::uuid), '[]'::jsonb), \
            'audit_count', (SELECT COUNT(*) FROM litecr.mutation_audit a WHERE a.resource_ref=$2), \
            'outbox_count', (SELECT COUNT(*) FROM litecr.outbox_event o WHERE o.aggregate_id=$2::uuid), \
            'reverse_receipts', COALESCE((SELECT jsonb_agg(to_jsonb(r)) FROM litecr.migration_reverse_receipt r WHERE r.manifest_sha256=$1), '[]'::jsonb)\
        )",
    )
    .bind(manifest_sha256.to_vec())
    .bind(source_record_id)
    .fetch_one(pool)
    .await
    .map_err(|_| "source and reverse side-effect snapshot queried".to_owned())
}

fn ensure(condition: bool, message: &str) -> Result<(), String> {
    condition.then_some(()).ok_or_else(|| message.to_owned())
}
