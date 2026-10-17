//! Server-owned exact-byte staging for the additive migration package v2 API.

use litecr_application::{
    access::{AccessContext, Permission, ResourceScope, authorize},
    migration::{
        MAX_API_BATCH_BYTES, MAX_BATCH_RECORDS, MAX_RECORD_BYTES, MigrationError,
        MigrationPackagePolicy, MigrationPackageV2Receipt,
        package::{self, PackageStreamVerifier, ParsedManifest},
    },
};
use sha2::{Digest, Sha256};
use sqlx::{Postgres, Row, Transaction};

use super::{PostgresMigrationExecutor, map_supported_record};

const EXPIRY_CLEANUP_BATCH_SIZE: u32 = 100;

fn validate_policy(policy: &MigrationPackagePolicy) -> Result<(), MigrationError> {
    if policy.global_quota_bytes == 0
        || policy.max_active_uploads == 0
        || policy.retention_seconds == 0
        || policy.storage_protection_reference.trim().is_empty()
        || policy.storage_protection_reference.len() > 512
        || policy
            .storage_protection_reference
            .chars()
            .any(char::is_control)
    {
        return Err(MigrationError::PackageV2Disabled);
    }
    Ok(())
}

fn actor_id(context: &AccessContext) -> String {
    context.actor().id.clone()
}

fn run_scope(run: &litecr_application::migration::MigrationRun) -> ResourceScope {
    ResourceScope {
        registry_id: run.registry_id.clone(),
        organization_id: run.organization_id.clone(),
        record_id: None,
    }
}

pub(super) fn row_run(
    row: &sqlx::postgres::PgRow,
) -> Result<litecr_application::migration::MigrationRun, MigrationError> {
    Ok(litecr_application::migration::MigrationRun {
        manifest_sha256: row
            .try_get::<Vec<u8>, _>("manifest_sha256")
            .map_err(|_| MigrationError::PackageNotFound)?
            .try_into()
            .map_err(|_| MigrationError::PackageVerificationFailed)?,
        stream_sha256: row
            .try_get::<Vec<u8>, _>("stream_sha256")
            .map_err(|_| MigrationError::PackageNotFound)?
            .try_into()
            .map_err(|_| MigrationError::PackageVerificationFailed)?,
        snapshot_id: row
            .try_get("snapshot_id")
            .map_err(|_| MigrationError::PackageNotFound)?,
        source_system_id: row
            .try_get("source_system_id")
            .map_err(|_| MigrationError::PackageNotFound)?,
        registry_id: row
            .try_get("registry_id")
            .map_err(|_| MigrationError::PackageNotFound)?,
        organization_id: row
            .try_get("organization_id")
            .map_err(|_| MigrationError::PackageNotFound)?,
        expected_record_count: row
            .try_get::<i64, _>("expected_record_count")
            .map_err(|_| MigrationError::PackageNotFound)? as u64,
    })
}

pub(super) fn receipt(
    row: &sqlx::postgres::PgRow,
) -> Result<MigrationPackageV2Receipt, MigrationError> {
    Ok(MigrationPackageV2Receipt {
        upload_id: row
            .try_get("upload_id_text")
            .map_err(|_| MigrationError::PackageNotFound)?,
        manifest_sha256: row
            .try_get::<Vec<u8>, _>("manifest_sha256")
            .map_err(|_| MigrationError::PackageNotFound)?
            .try_into()
            .map_err(|_| MigrationError::PackageVerificationFailed)?,
        stream_sha256: row
            .try_get::<Vec<u8>, _>("stream_sha256")
            .map_err(|_| MigrationError::PackageNotFound)?
            .try_into()
            .map_err(|_| MigrationError::PackageVerificationFailed)?,
        state: row
            .try_get("state")
            .map_err(|_| MigrationError::PackageNotFound)?,
        staged_batch_count: row
            .try_get::<i64, _>("staged_batch_count")
            .map_err(|_| MigrationError::PackageNotFound)? as u64,
        staged_bytes: row
            .try_get::<i64, _>("staged_bytes")
            .map_err(|_| MigrationError::PackageNotFound)? as u64,
        record_count: row
            .try_get::<i64, _>("staged_record_count")
            .map_err(|_| MigrationError::PackageNotFound)? as u64,
        committed_batch_sequence: row
            .try_get::<i64, _>("committed_batch_sequence")
            .map_err(|_| MigrationError::PackageNotFound)? as u64,
        committed_record_count: row
            .try_get::<i64, _>("committed_record_count")
            .map_err(|_| MigrationError::PackageNotFound)? as u64,
    })
}

fn upload_uuid(upload_id: &str) -> Result<String, MigrationError> {
    if upload_id.len() != 36
        || !upload_id.bytes().enumerate().all(|(i, b)| {
            if [8, 13, 18, 23].contains(&i) {
                b == b'-'
            } else {
                b.is_ascii_hexdigit()
            }
        })
    {
        return Err(MigrationError::PackageNotFound);
    }
    Ok(upload_id.to_owned())
}

pub(super) async fn lock_authorized_upload(
    tx: &mut Transaction<'_, Postgres>,
    context: &AccessContext,
    upload_id: &str,
) -> Result<
    (
        String,
        litecr_application::migration::MigrationRun,
        sqlx::postgres::PgRow,
    ),
    MigrationError,
> {
    let id = upload_uuid(upload_id)?;
    let row = sqlx::query("SELECT *,upload_id::text AS upload_id_text,(expires_at > now()) AS is_unexpired FROM litecr.migration_package_v2 WHERE upload_id=$1::uuid FOR UPDATE")
        .bind(&id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(|_| MigrationError::CommitFailed)?
        .ok_or(MigrationError::PackageNotFound)?;
    let run = row_run(&row)?;
    authorize(Some(context), Permission::MigrationWrite, &run_scope(&run))?;
    Ok((id, run, row))
}

impl PostgresMigrationExecutor {
    pub async fn cleanup_expired_packages_v2(&self, limit: u32) -> Result<u64, MigrationError> {
        if limit == 0 || limit > 1000 {
            return Err(MigrationError::PackageQuotaExceeded);
        }
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|_| MigrationError::CommitFailed)?;
        lock_global_quota(&mut tx).await?;
        let expired = expire_staged_packages(&mut tx, limit).await?;
        tx.commit()
            .await
            .map_err(|_| MigrationError::CommitFailed)?;
        Ok(expired)
    }

    pub(super) async fn create_package_v2(
        &self,
        context: &AccessContext,
        manifest_bytes: &[u8],
        policy: &MigrationPackagePolicy,
    ) -> Result<MigrationPackageV2Receipt, MigrationError> {
        validate_policy(policy)?;
        let parsed = package::parse_manifest(manifest_bytes)
            .map_err(|_| MigrationError::PackageVerificationFailed)?;
        authorize(
            Some(context),
            Permission::MigrationWrite,
            &run_scope(&parsed.run),
        )?;
        let actor = actor_id(context);
        let manifest_hash = parsed.run.manifest_sha256;
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|_| MigrationError::CommitFailed)?;
        lock_global_quota(&mut tx).await?;
        expire_staged_packages(&mut tx, EXPIRY_CLEANUP_BATCH_SIZE).await?;
        let prior = sqlx::query("SELECT *,upload_id::text AS upload_id_text FROM litecr.migration_package_v2 WHERE manifest_sha256=$1 FOR UPDATE")
            .bind(manifest_hash.to_vec())
            .fetch_optional(&mut *tx)
            .await
            .map_err(|_| MigrationError::CommitFailed)?;
        if let Some(row) = prior {
            if row.try_get::<String, _>("registry_id").ok().as_deref()
                != Some(parsed.run.registry_id.as_str())
                || row.try_get::<String, _>("organization_id").ok().as_deref()
                    != Some(parsed.run.organization_id.as_str())
                || row.try_get::<Vec<u8>, _>("stream_sha256").ok().as_deref()
                    != Some(parsed.run.stream_sha256.as_slice())
            {
                return Err(MigrationError::PackageConflict);
            }
            if row.try_get::<String, _>("state").ok().as_deref() == Some("expired") {
                let committed_batches = row
                    .try_get::<i64, _>("committed_batch_sequence")
                    .map_err(|_| MigrationError::PackageConflict)?;
                let committed_records = row
                    .try_get::<i64, _>("committed_record_count")
                    .map_err(|_| MigrationError::PackageConflict)?;
                let has_staged_batches: bool = sqlx::query_scalar(
                    "SELECT EXISTS(SELECT 1 FROM litecr.migration_package_v2_batch WHERE upload_id=$1::uuid)",
                )
                .bind(row.try_get::<String, _>("upload_id_text").map_err(|_| MigrationError::PackageNotFound)?)
                .fetch_one(&mut *tx)
                .await
                .map_err(|_| MigrationError::CommitFailed)?;
                if committed_batches != 0 || committed_records != 0 || has_staged_batches {
                    return Err(MigrationError::PackageConflict);
                }
                ensure_quota(&mut tx, parsed.stream_bytes, policy.global_quota_bytes).await?;
                ensure_active_upload_capacity(&mut tx, policy.max_active_uploads).await?;
                let upload_id: String = row
                    .try_get("upload_id_text")
                    .map_err(|_| MigrationError::PackageNotFound)?;
                let reopened = sqlx::query(
                    "UPDATE litecr.migration_package_v2 \
                     SET state='receiving',staged_batch_count=0,staged_bytes=0, \
                         reserved_bytes=expected_stream_bytes,verified_stream_bytes=0, \
                         staged_record_count=0,storage_protection_reference=$3, \
                         expires_at=now()+($2::bigint * interval '1 second'), \
                         updated_at=now() \
                     WHERE upload_id=$1::uuid AND state='expired' \
                       AND committed_batch_sequence=0 AND committed_record_count=0 \
                     RETURNING *,upload_id::text AS upload_id_text",
                )
                .bind(&upload_id)
                .bind(policy.retention_seconds as i64)
                .bind(&policy.storage_protection_reference)
                .fetch_optional(&mut *tx)
                .await
                .map_err(|_| MigrationError::CommitFailed)?
                .ok_or(MigrationError::PackageConflict)?;
                tx.commit()
                    .await
                    .map_err(|_| MigrationError::CommitFailed)?;
                return receipt(&reopened);
            }
            let result = receipt(&row)?;
            tx.commit()
                .await
                .map_err(|_| MigrationError::CommitFailed)?;
            return Ok(result);
        }
        ensure_quota(&mut tx, parsed.stream_bytes, policy.global_quota_bytes).await?;
        ensure_active_upload_capacity(&mut tx, policy.max_active_uploads).await?;
        let row = sqlx::query("INSERT INTO litecr.migration_package_v2 (manifest_sha256,stream_sha256,snapshot_id,source_system_id,registry_id,organization_id,expected_record_count,expected_stream_bytes,state,staged_bytes,reserved_bytes,storage_protection_reference,created_by,expires_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,'receiving',$9,$10,$11,$12,now()+($13::bigint * interval '1 second')) RETURNING *,upload_id::text AS upload_id_text")
            .bind(manifest_hash.to_vec()).bind(parsed.run.stream_sha256.to_vec())
            .bind(&parsed.run.snapshot_id).bind(&parsed.run.source_system_id).bind(&parsed.run.registry_id)
            .bind(&parsed.run.organization_id).bind(parsed.run.expected_record_count as i64)
            .bind(parsed.stream_bytes as i64).bind(0_i64).bind(parsed.stream_bytes as i64)
            .bind(&policy.storage_protection_reference).bind(actor).bind(policy.retention_seconds as i64)
            .fetch_one(&mut *tx).await.map_err(|_| MigrationError::CommitFailed)?;
        tx.commit()
            .await
            .map_err(|_| MigrationError::CommitFailed)?;
        receipt(&row)
    }

    pub(super) async fn stage_package_v2_batch(
        &self,
        context: &AccessContext,
        upload_id: &str,
        sequence: u64,
        exact_ndjson: &[u8],
        policy: &MigrationPackagePolicy,
    ) -> Result<MigrationPackageV2Receipt, MigrationError> {
        validate_policy(policy)?;
        if sequence == 0 || sequence > i64::MAX as u64 || exact_ndjson.len() > MAX_API_BATCH_BYTES {
            return Err(MigrationError::InvalidBatch);
        }
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|_| MigrationError::CommitFailed)?;
        lock_global_quota(&mut tx).await?;
        expire_staged_packages(&mut tx, EXPIRY_CLEANUP_BATCH_SIZE).await?;
        let (id, run, parent) = lock_authorized_upload(&mut tx, context, upload_id).await?;
        if parent.try_get::<String, _>("state").ok().as_deref() == Some("expired") {
            tx.commit()
                .await
                .map_err(|_| MigrationError::CommitFailed)?;
            return Err(MigrationError::PackageConflict);
        }
        if !parent.try_get::<bool, _>("is_unexpired").unwrap_or(false) {
            expire_one_package(&mut tx, &id).await?;
            tx.commit()
                .await
                .map_err(|_| MigrationError::CommitFailed)?;
            return Err(MigrationError::PackageConflict);
        }
        if parent.try_get::<String, _>("state").ok().as_deref() != Some("receiving") {
            return Err(MigrationError::PackageConflict);
        }
        let digest = Sha256::digest(exact_ndjson).to_vec();
        let prior = sqlx::query("SELECT ndjson_bytes,batch_sha256 FROM litecr.migration_package_v2_batch WHERE upload_id=$1::uuid AND batch_sequence=$2")
            .bind(&id).bind(sequence as i64).fetch_optional(&mut *tx).await.map_err(|_| MigrationError::CommitFailed)?;
        if let Some(prior) = prior {
            if prior.try_get::<Vec<u8>, _>("ndjson_bytes").ok().as_deref() != Some(exact_ndjson)
                || prior.try_get::<Vec<u8>, _>("batch_sha256").ok().as_deref()
                    != Some(digest.as_slice())
            {
                return Err(MigrationError::PackageConflict);
            }
            let row = sqlx::query("SELECT *,upload_id::text AS upload_id_text FROM litecr.migration_package_v2 WHERE upload_id=$1::uuid")
                .bind(&id).fetch_one(&mut *tx).await.map_err(|_| MigrationError::CommitFailed)?;
            tx.commit()
                .await
                .map_err(|_| MigrationError::CommitFailed)?;
            return receipt(&row);
        }
        let expected_sequence = parent
            .try_get::<i64, _>("staged_batch_count")
            .map_err(|_| MigrationError::PackageNotFound)? as u64
            + 1;
        if sequence != expected_sequence {
            return Err(MigrationError::PackageConflict);
        }
        let current_bytes = parent
            .try_get::<i64, _>("staged_bytes")
            .map_err(|_| MigrationError::PackageNotFound)? as u64;
        let first_line = parent
            .try_get::<i64, _>("staged_record_count")
            .map_err(|_| MigrationError::PackageNotFound)? as u64
            + 1;
        let staged_batch = parse_exact_batch(&run, sequence, first_line, exact_ndjson)?;
        for record in &staged_batch.records {
            map_supported_record(&run, record).map_err(|_| MigrationError::UnsupportedRecord)?;
        }
        let new_bytes = current_bytes
            .checked_add(exact_ndjson.len() as u64)
            .ok_or(MigrationError::PackageQuotaExceeded)?;
        let new_records = first_line - 1 + staged_batch.records.len() as u64;
        let declared_bytes = parent
            .try_get::<i64, _>("expected_stream_bytes")
            .map_err(|_| MigrationError::PackageNotFound)? as u64;
        if new_bytes > declared_bytes || new_records > run.expected_record_count {
            return Err(MigrationError::PackageVerificationFailed);
        }
        sqlx::query("INSERT INTO litecr.migration_package_v2_batch (upload_id,batch_sequence,ndjson_bytes,batch_sha256,first_input_line,last_input_line,record_count) VALUES ($1::uuid,$2,$3,$4,$5,$6,$7)")
            .bind(&id).bind(sequence as i64).bind(exact_ndjson).bind(digest)
            .bind(first_line as i64).bind((new_records) as i64).bind(staged_batch.records.len() as i32)
            .execute(&mut *tx).await.map_err(|_| MigrationError::CommitFailed)?;
        sqlx::query("UPDATE litecr.migration_package_v2 SET staged_batch_count=$2,staged_bytes=$3,staged_record_count=$4,updated_at=now() WHERE upload_id=$1::uuid")
            .bind(&id).bind(sequence as i64).bind(new_bytes as i64).bind(new_records as i64)
            .execute(&mut *tx).await.map_err(|_| MigrationError::CommitFailed)?;
        let row = sqlx::query("SELECT *,upload_id::text AS upload_id_text FROM litecr.migration_package_v2 WHERE upload_id=$1::uuid")
            .bind(&id).fetch_one(&mut *tx).await.map_err(|_| MigrationError::CommitFailed)?;
        tx.commit()
            .await
            .map_err(|_| MigrationError::CommitFailed)?;
        receipt(&row)
    }

    pub(super) async fn finalize_package_v2(
        &self,
        context: &AccessContext,
        upload_id: &str,
        policy: &MigrationPackagePolicy,
    ) -> Result<MigrationPackageV2Receipt, MigrationError> {
        validate_policy(policy)?;
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|_| MigrationError::CommitFailed)?;
        let (id, run, row) = lock_authorized_upload(&mut tx, context, upload_id).await?;
        let state: String = row
            .try_get("state")
            .map_err(|_| MigrationError::PackageNotFound)?;
        if state == "ready" || state == "applying" || state == "completed" {
            let value = receipt(&row)?;
            tx.commit()
                .await
                .map_err(|_| MigrationError::CommitFailed)?;
            return Ok(value);
        }
        if state == "rejected" || state == "expired" {
            return Err(MigrationError::PackageConflict);
        }
        if !row.try_get::<bool, _>("is_unexpired").unwrap_or(false) {
            expire_one_package(&mut tx, &id).await?;
            tx.commit()
                .await
                .map_err(|_| MigrationError::CommitFailed)?;
            return Err(MigrationError::PackageConflict);
        }
        if state != "receiving" && state != "verifying" {
            return Err(MigrationError::PackageConflict);
        }
        sqlx::query("UPDATE litecr.migration_package_v2 SET state='verifying',updated_at=now() WHERE upload_id=$1::uuid")
            .bind(&id).execute(&mut *tx).await.map_err(|_| MigrationError::CommitFailed)?;
        tx.commit()
            .await
            .map_err(|_| MigrationError::CommitFailed)?;

        let stream_bytes = row
            .try_get::<i64, _>("expected_stream_bytes")
            .map_err(|_| MigrationError::PackageNotFound)? as u64;
        let mut verifier = PackageStreamVerifier::from_parsed_manifest(ParsedManifest {
            run: run.clone(),
            stream_bytes,
            exported_at: String::new(),
        });
        let staged_count: u64 = row
            .try_get::<i64, _>("staged_batch_count")
            .map_err(|_| MigrationError::PackageNotFound)? as u64;
        let mut valid = true;
        let mut expected_line = 1_u64;
        for seq in 1..=staged_count {
            let batch = sqlx::query("SELECT ndjson_bytes,batch_sha256,first_input_line,last_input_line,record_count FROM litecr.migration_package_v2_batch WHERE upload_id=$1::uuid AND batch_sequence=$2")
                .bind(&id).bind(seq as i64).fetch_optional(&self.pool).await.map_err(|_| MigrationError::CommitFailed)?;
            let Some(batch) = batch else {
                valid = false;
                break;
            };
            let bytes: Vec<u8> = batch
                .try_get("ndjson_bytes")
                .map_err(|_| MigrationError::PackageVerificationFailed)?;
            let digest: Vec<u8> = batch
                .try_get("batch_sha256")
                .map_err(|_| MigrationError::PackageVerificationFailed)?;
            if Sha256::digest(&bytes).as_slice() != digest.as_slice()
                || batch.try_get::<i64, _>("first_input_line").ok() != Some(expected_line as i64)
            {
                valid = false;
                break;
            }
            let parsed = match parse_exact_batch(&run, seq, expected_line, &bytes) {
                Ok(parsed) => parsed,
                Err(_) => {
                    valid = false;
                    break;
                }
            };
            if parsed
                .records
                .iter()
                .any(|record| map_supported_record(&run, record).is_err())
            {
                valid = false;
                break;
            }
            if batch.try_get::<i64, _>("last_input_line").ok()
                != Some((expected_line + parsed.records.len() as u64 - 1) as i64)
                || batch.try_get::<i32, _>("record_count").ok() != Some(parsed.records.len() as i32)
            {
                valid = false;
                break;
            }
            expected_line += parsed.records.len() as u64;
            if verifier.update(&bytes).is_err() {
                valid = false;
                break;
            }
        }
        if verifier.finish().is_err() {
            valid = false;
        }
        if expected_line - 1 != run.expected_record_count {
            valid = false;
        }
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|_| MigrationError::CommitFailed)?;
        let (_, _, current) = lock_authorized_upload(&mut tx, context, upload_id).await?;
        let current_state: String = current
            .try_get("state")
            .map_err(|_| MigrationError::PackageNotFound)?;
        if current_state == "ready" {
            let result = receipt(&current)?;
            tx.commit()
                .await
                .map_err(|_| MigrationError::CommitFailed)?;
            return Ok(result);
        }
        if current_state != "verifying" {
            return Err(MigrationError::PackageConflict);
        }
        let next_state = if valid { "ready" } else { "rejected" };
        sqlx::query("UPDATE litecr.migration_package_v2 SET state=$2,updated_at=now() WHERE upload_id=$1::uuid")
            .bind(&id).bind(next_state).execute(&mut *tx).await.map_err(|_| MigrationError::CommitFailed)?;
        if !valid {
            sqlx::query("DELETE FROM litecr.migration_package_v2_batch WHERE upload_id=$1::uuid")
                .bind(&id)
                .execute(&mut *tx)
                .await
                .map_err(|_| MigrationError::CommitFailed)?;
            sqlx::query("UPDATE litecr.migration_package_v2 SET staged_bytes=0,reserved_bytes=0,updated_at=now() WHERE upload_id=$1::uuid")
                .bind(&id)
                .execute(&mut *tx)
                .await
                .map_err(|_| MigrationError::CommitFailed)?;
        }
        let updated = sqlx::query("SELECT *,upload_id::text AS upload_id_text FROM litecr.migration_package_v2 WHERE upload_id=$1::uuid")
            .bind(&id).fetch_one(&mut *tx).await.map_err(|_| MigrationError::CommitFailed)?;
        tx.commit()
            .await
            .map_err(|_| MigrationError::CommitFailed)?;
        if !valid {
            return Err(MigrationError::PackageVerificationFailed);
        }
        receipt(&updated)
    }

    pub(super) async fn inspect_package_v2(
        &self,
        context: &AccessContext,
        upload_id: &str,
    ) -> Result<MigrationPackageV2Receipt, MigrationError> {
        let id = upload_uuid(upload_id)?;
        let row = sqlx::query("SELECT *,upload_id::text AS upload_id_text FROM litecr.migration_package_v2 WHERE upload_id=$1::uuid")
            .bind(&id).fetch_optional(&self.pool).await.map_err(|_| MigrationError::CommitFailed)?
            .ok_or(MigrationError::PackageNotFound)?;
        let run = row_run(&row)?;
        authorize(Some(context), Permission::MigrationRead, &run_scope(&run))?;
        receipt(&row)
    }
}

async fn lock_global_quota(tx: &mut Transaction<'_, Postgres>) -> Result<(), MigrationError> {
    sqlx::query("SELECT pg_advisory_xact_lock(768421905)")
        .execute(&mut **tx)
        .await
        .map_err(|_| MigrationError::CommitFailed)?;
    Ok(())
}

/// Expire incomplete or verified-but-unapplied uploads before quota is counted.
/// The configured TTL is persisted per package; no process default is assumed.
async fn expire_staged_packages(
    tx: &mut Transaction<'_, Postgres>,
    limit: u32,
) -> Result<u64, MigrationError> {
    let expired: i64 = sqlx::query_scalar(
        "WITH candidates AS ( \
             SELECT upload_id FROM litecr.migration_package_v2 \
             WHERE state IN ('receiving','verifying','ready') AND expires_at <= now() \
             ORDER BY expires_at,upload_id LIMIT $1 FOR UPDATE SKIP LOCKED \
         ), expired AS ( \
             UPDATE litecr.migration_package_v2 AS p \
             SET state='expired',staged_bytes=0,reserved_bytes=0,updated_at=now() \
             FROM candidates c WHERE p.upload_id=c.upload_id \
             RETURNING p.upload_id \
         ), deleted AS ( \
             DELETE FROM litecr.migration_package_v2_batch b \
             USING expired e WHERE b.upload_id=e.upload_id RETURNING 1 \
         ) SELECT count(*)::bigint FROM expired",
    )
    .bind(i64::from(limit))
    .fetch_one(&mut **tx)
    .await
    .map_err(|_| MigrationError::CommitFailed)?;
    Ok(expired as u64)
}

async fn expire_one_package(
    tx: &mut Transaction<'_, Postgres>,
    upload_id: &str,
) -> Result<(), MigrationError> {
    sqlx::query("UPDATE litecr.migration_package_v2 SET state='expired',staged_bytes=0,reserved_bytes=0,updated_at=now() WHERE upload_id=$1::uuid")
        .bind(upload_id)
        .execute(&mut **tx)
        .await
        .map_err(|_| MigrationError::CommitFailed)?;
    sqlx::query("DELETE FROM litecr.migration_package_v2_batch WHERE upload_id=$1::uuid")
        .bind(upload_id)
        .execute(&mut **tx)
        .await
        .map_err(|_| MigrationError::CommitFailed)?;
    Ok(())
}

async fn ensure_quota(
    tx: &mut Transaction<'_, Postgres>,
    added: u64,
    quota: u64,
) -> Result<(), MigrationError> {
    let current: i64 = sqlx::query_scalar("SELECT COALESCE(sum(reserved_bytes),0)::bigint FROM litecr.migration_package_v2 WHERE state NOT IN ('completed','rejected','expired')")
        .fetch_one(&mut **tx).await.map_err(|_| MigrationError::CommitFailed)?;
    if (current as u64)
        .checked_add(added)
        .is_none_or(|total| total > quota)
    {
        return Err(MigrationError::PackageQuotaExceeded);
    }
    Ok(())
}

async fn ensure_active_upload_capacity(
    tx: &mut Transaction<'_, Postgres>,
    maximum: u32,
) -> Result<(), MigrationError> {
    let active: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM litecr.migration_package_v2 \
         WHERE state IN ('receiving','verifying','ready','applying')",
    )
    .fetch_one(&mut **tx)
    .await
    .map_err(|_| MigrationError::CommitFailed)?;
    if active >= i64::from(maximum) {
        return Err(MigrationError::PackageQuotaExceeded);
    }
    Ok(())
}

pub(super) fn parse_exact_batch(
    run: &litecr_application::migration::MigrationRun,
    sequence: u64,
    first_input_line: u64,
    exact_ndjson: &[u8],
) -> Result<litecr_application::migration::MigrationBatch, MigrationError> {
    use litecr_application::migration::{MigrationBatch, MigrationRecord};
    if exact_ndjson.is_empty()
        || exact_ndjson.len() > MAX_API_BATCH_BYTES
        || !exact_ndjson.ends_with(b"\n")
    {
        return Err(MigrationError::InvalidBatch);
    }
    let mut records = Vec::new();
    for line in exact_ndjson.split_inclusive(|byte| *byte == b'\n') {
        if line.len() < 2 || line.len() > MAX_RECORD_BYTES || line.last() != Some(&b'\n') {
            return Err(MigrationError::RecordTooLarge);
        }
        let row: serde_json::Value = serde_json::from_slice(&line[..line.len() - 1])
            .map_err(|_| MigrationError::InvalidBatch)?;
        let get = |key: &str| {
            row.get(key)
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
                .ok_or(MigrationError::InvalidBatch)
        };
        let source_system_id = get("source_system_id")?;
        let source_record_key = get("source_record_key")?;
        let source_version = get("source_version")?;
        let canonical = serde_json::to_vec(&row).map_err(|_| MigrationError::InvalidBatch)?;
        let payload_sha256: [u8; 32] = Sha256::digest(canonical).into();
        records.push(MigrationRecord {
            source_system_id,
            source_record_key,
            source_version,
            payload_sha256,
            raw_ndjson: line.to_vec(),
        });
        if records.len() > MAX_BATCH_RECORDS {
            return Err(MigrationError::InvalidBatch);
        }
    }
    let last_input_line = first_input_line
        .checked_add(records.len() as u64)
        .and_then(|n| n.checked_sub(1))
        .ok_or(MigrationError::InvalidBatch)?;
    let batch = MigrationBatch {
        run: run.clone(),
        sequence,
        first_input_line,
        last_input_line,
        batch_sha256: Sha256::digest(exact_ndjson).into(),
        exact_bytes: exact_ndjson.to_vec(),
        records,
    };
    litecr_application::migration::validate_batch(&batch)?;
    Ok(batch)
}
