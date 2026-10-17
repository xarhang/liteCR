//! Dedicated v1 Registry writer. It writes only model-supported source fields
//! and is deliberately transaction-scoped so the migration journal, Registry
//! state, idempotency, audit, and outbox commit together.

use crate::matching::index_accepted_source_version;
use async_trait::async_trait;
use litecr_application::{
    access::{AccessContext, ActorKind, Permission, ResourceScope, authorize},
    migration::{
        AppliedSource, BatchCommit, MigrationBatch, MigrationCheckpoint, MigrationError,
        MigrationExecutor, MigrationPackagePolicy, MigrationPackageV2Receipt, MigrationRun,
        MigrationRunState, QuarantineDisposition, QuarantineDispositionAction,
        QuarantineDispositionReceipt, ReverseBlockerCode, ReverseBlockerCount, ReverseManifest,
        ReversePreflight, ReversePreflightRequest, ReverseReceipt,
    },
    registry::RegistryActor,
};
use litecr_domain::registry::SourceRecordCommand;
use serde_json::json;
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Postgres, Row};
use std::collections::BTreeMap;
use std::fmt::Write as _;

use super::{MappedMigrationSource, PostgresMigrationJournal, map_supported_record};

#[derive(Clone)]
pub struct PostgresMigrationExecutor {
    pub(super) pool: PgPool,
    journal: PostgresMigrationJournal,
}

impl PostgresMigrationExecutor {
    pub fn new(pool: PgPool) -> Self {
        Self {
            journal: PostgresMigrationJournal::new(pool.clone()),
            pool,
        }
    }

    async fn apply_verified_package_batch(
        &self,
        context: &AccessContext,
        upload_id: &str,
    ) -> Result<MigrationPackageV2Receipt, MigrationError> {
        use super::package_v2::{lock_authorized_upload, parse_exact_batch, receipt};
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|_| MigrationError::CommitFailed)?;
        let (id, run, package) = lock_authorized_upload(&mut tx, context, upload_id).await?;
        let state: String = package
            .try_get("state")
            .map_err(|_| MigrationError::PackageNotFound)?;
        if state == "completed" {
            let result = receipt(&package)?;
            tx.commit()
                .await
                .map_err(|_| MigrationError::CommitFailed)?;
            return Ok(result);
        }
        if state == "ready" && !package.try_get::<bool, _>("is_unexpired").unwrap_or(false) {
            sqlx::query("UPDATE litecr.migration_package_v2 SET state='expired',staged_bytes=0,reserved_bytes=0,updated_at=now() WHERE upload_id=$1::uuid")
                .bind(&id)
                .execute(&mut *tx)
                .await
                .map_err(|_| MigrationError::CommitFailed)?;
            sqlx::query("DELETE FROM litecr.migration_package_v2_batch WHERE upload_id=$1::uuid")
                .bind(&id)
                .execute(&mut *tx)
                .await
                .map_err(|_| MigrationError::CommitFailed)?;
            tx.commit()
                .await
                .map_err(|_| MigrationError::CommitFailed)?;
            return Err(MigrationError::PackageConflict);
        }
        if state != "ready" && state != "applying" {
            return Err(MigrationError::PackageConflict);
        }

        let actor = registry_actor(context);
        let checkpoint =
            PostgresMigrationJournal::establish_in_transaction(&mut tx, &run, &actor.id).await?;
        let package_sequence = package
            .try_get::<i64, _>("committed_batch_sequence")
            .map_err(|_| MigrationError::PackageNotFound)? as u64;
        let package_records = package
            .try_get::<i64, _>("committed_record_count")
            .map_err(|_| MigrationError::PackageNotFound)? as u64;
        if checkpoint.committed_batch_sequence != package_sequence
            || checkpoint.committed_record_count != package_records
        {
            // A historical v1 checkpoint is not adoptable as a server-verified v2 upload.
            return Err(MigrationError::PackageConflict);
        }
        if checkpoint.committed_batch_sequence == 0
            && checkpoint.state != MigrationRunState::Validated
        {
            return Err(MigrationError::PackageConflict);
        }
        let next = checkpoint.committed_batch_sequence + 1;
        let staged_count = package
            .try_get::<i64, _>("staged_batch_count")
            .map_err(|_| MigrationError::PackageNotFound)? as u64;
        if next > staged_count {
            if checkpoint.committed_record_count != run.expected_record_count {
                return Err(MigrationError::PackageVerificationFailed);
            }
            sqlx::query("UPDATE litecr.migration_package_v2 SET state='completed',updated_at=now(),staged_bytes=0,reserved_bytes=0 WHERE upload_id=$1::uuid")
                .bind(&id).execute(&mut *tx).await.map_err(|_| MigrationError::CommitFailed)?;
            sqlx::query("DELETE FROM litecr.migration_package_v2_batch WHERE upload_id=$1::uuid")
                .bind(&id)
                .execute(&mut *tx)
                .await
                .map_err(|_| MigrationError::CommitFailed)?;
            let updated = sqlx::query("SELECT *,upload_id::text AS upload_id_text FROM litecr.migration_package_v2 WHERE upload_id=$1::uuid")
                .bind(&id).fetch_one(&mut *tx).await.map_err(|_| MigrationError::CommitFailed)?;
            tx.commit()
                .await
                .map_err(|_| MigrationError::CommitFailed)?;
            return receipt(&updated);
        }
        let stored = sqlx::query("SELECT ndjson_bytes,batch_sha256,first_input_line FROM litecr.migration_package_v2_batch WHERE upload_id=$1::uuid AND batch_sequence=$2")
            .bind(&id).bind(next as i64).fetch_optional(&mut *tx).await.map_err(|_| MigrationError::CommitFailed)?
            .ok_or(MigrationError::PackageVerificationFailed)?;
        let raw: Vec<u8> = stored
            .try_get("ndjson_bytes")
            .map_err(|_| MigrationError::PackageVerificationFailed)?;
        let digest: Vec<u8> = stored
            .try_get("batch_sha256")
            .map_err(|_| MigrationError::PackageVerificationFailed)?;
        let batch = parse_exact_batch(
            &run,
            next,
            stored
                .try_get::<i64, _>("first_input_line")
                .map_err(|_| MigrationError::PackageVerificationFailed)? as u64,
            &raw,
        )?;
        if Sha256::digest(&raw).as_slice() != digest.as_slice() {
            return Err(MigrationError::PackageVerificationFailed);
        }
        let mapped = batch
            .records
            .iter()
            .map(|record| {
                super::map_supported_record(&batch.run, record)
                    .map_err(|_| MigrationError::UnsupportedRecord)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut applied = Vec::with_capacity(batch.records.len());
        for (record, mapped_row) in batch.records.iter().zip(mapped.iter()) {
            applied.push(import_one(&mut tx, &actor, &batch.run, record, mapped_row).await?);
        }
        let commit = PostgresMigrationJournal::record_batch_in_transaction(
            &mut tx, &batch, &actor.id, &applied,
        )
        .await?;
        let completed = commit.checkpoint.committed_record_count == run.expected_record_count;
        sqlx::query("UPDATE litecr.migration_package_v2 SET state=$2,committed_batch_sequence=$3,committed_record_count=$4,updated_at=now(),staged_bytes=CASE WHEN $2='completed' THEN 0 ELSE staged_bytes END,reserved_bytes=CASE WHEN $2='completed' THEN 0 ELSE reserved_bytes END WHERE upload_id=$1::uuid")
            .bind(&id).bind(if completed { "completed" } else { "applying" })
            .bind(commit.checkpoint.committed_batch_sequence as i64).bind(commit.checkpoint.committed_record_count as i64)
            .execute(&mut *tx).await.map_err(|_| MigrationError::CommitFailed)?;
        if completed {
            sqlx::query("DELETE FROM litecr.migration_package_v2_batch WHERE upload_id=$1::uuid")
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
        receipt(&updated)
    }
}

#[async_trait]
impl MigrationExecutor for PostgresMigrationExecutor {
    async fn create_package_v2(
        &self,
        context: &AccessContext,
        manifest_bytes: &[u8],
        policy: &MigrationPackagePolicy,
    ) -> Result<MigrationPackageV2Receipt, MigrationError> {
        PostgresMigrationExecutor::create_package_v2(self, context, manifest_bytes, policy).await
    }

    async fn stage_package_v2_batch(
        &self,
        context: &AccessContext,
        upload_id: &str,
        sequence: u64,
        exact_ndjson: &[u8],
        policy: &MigrationPackagePolicy,
    ) -> Result<MigrationPackageV2Receipt, MigrationError> {
        PostgresMigrationExecutor::stage_package_v2_batch(
            self,
            context,
            upload_id,
            sequence,
            exact_ndjson,
            policy,
        )
        .await
    }

    async fn finalize_package_v2(
        &self,
        context: &AccessContext,
        upload_id: &str,
        policy: &MigrationPackagePolicy,
    ) -> Result<MigrationPackageV2Receipt, MigrationError> {
        PostgresMigrationExecutor::finalize_package_v2(self, context, upload_id, policy).await
    }

    async fn inspect_package_v2(
        &self,
        context: &AccessContext,
        upload_id: &str,
    ) -> Result<MigrationPackageV2Receipt, MigrationError> {
        PostgresMigrationExecutor::inspect_package_v2(self, context, upload_id).await
    }

    async fn apply_package_v2_batch(
        &self,
        context: &AccessContext,
        upload_id: &str,
    ) -> Result<MigrationPackageV2Receipt, MigrationError> {
        self.apply_verified_package_batch(context, upload_id).await
    }

    async fn record_quarantine_disposition(
        &self,
        context: &AccessContext,
        request: &QuarantineDisposition,
        idempotency_hash: [u8; 32],
        request_fingerprint: [u8; 32],
    ) -> Result<QuarantineDispositionReceipt, MigrationError> {
        let scope = ResourceScope {
            registry_id: request.registry_id.clone(),
            organization_id: request.organization_id.clone(),
            record_id: None,
        };
        authorize(Some(context), Permission::MigrationWrite, &scope)?;
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|_| MigrationError::CommitFailed)?;
        let actor = context.actor();
        let actor_kind = actor_kind_name(actor.kind);
        let issuer = actor.issuer_url.as_deref().unwrap_or_default();

        if let Some(prior) = sqlx::query(
            "SELECT disposition_id::text,request_fingerprint,correction_count,action,replacement_manifest_sha256 FROM litecr.migration_quarantine_disposition WHERE actor_kind=$1 AND actor_issuer_url=$2 AND actor_id=$3 AND registry_id=$4 AND organization_id=$5 AND idempotency_key_hash=$6 FOR UPDATE",
        )
        .bind(actor_kind)
        .bind(issuer)
        .bind(&actor.id)
        .bind(&request.registry_id)
        .bind(&request.organization_id)
        .bind(idempotency_hash.to_vec())
        .fetch_optional(&mut *tx)
        .await
        .map_err(|_| MigrationError::CommitFailed)?
        {
            let fingerprint: Vec<u8> = prior.try_get("request_fingerprint").map_err(|_| MigrationError::CommitFailed)?;
            if fingerprint.as_slice() != request_fingerprint {
                return Err(MigrationError::CheckpointConflict);
            }
            let receipt = decode_disposition_receipt(prior, true)?;
            tx.commit().await.map_err(|_| MigrationError::CommitFailed)?;
            return Ok(receipt);
        }

        let action = disposition_action_name(request.action);
        let inserted_id: Option<String> = sqlx::query_scalar(
            "INSERT INTO litecr.migration_quarantine_disposition (action,parent_manifest_sha256,replacement_manifest_sha256,registry_id,organization_id,actor_kind,actor_issuer_url,actor_id,reason,idempotency_key_hash,request_fingerprint,correction_count,request_id,correlation_id) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14) ON CONFLICT (actor_kind,actor_issuer_url,actor_id,registry_id,organization_id,idempotency_key_hash) DO NOTHING RETURNING disposition_id::text",
        )
        .bind(action)
        .bind(request.parent_manifest_sha256.to_vec())
        .bind(request.replacement_manifest_sha256.map(|digest| digest.to_vec()))
        .bind(&request.registry_id)
        .bind(&request.organization_id)
        .bind(actor_kind)
        .bind(issuer)
        .bind(&actor.id)
        .bind(&request.reason)
        .bind(idempotency_hash.to_vec())
        .bind(request_fingerprint.to_vec())
        .bind(request.corrections.len() as i32)
        .bind(context.request_id())
        .bind(context.correlation_id())
        .fetch_optional(&mut *tx)
        .await
        .map_err(|_| MigrationError::CommitFailed)?;
        let Some(disposition_id) = inserted_id else {
            let prior = sqlx::query("SELECT disposition_id::text,request_fingerprint,correction_count,action,replacement_manifest_sha256 FROM litecr.migration_quarantine_disposition WHERE actor_kind=$1 AND actor_issuer_url=$2 AND actor_id=$3 AND registry_id=$4 AND organization_id=$5 AND idempotency_key_hash=$6 FOR UPDATE")
                .bind(actor_kind)
                .bind(issuer)
                .bind(&actor.id)
                .bind(&request.registry_id)
                .bind(&request.organization_id)
                .bind(idempotency_hash.to_vec())
                .fetch_one(&mut *tx)
                .await
                .map_err(|_| MigrationError::CommitFailed)?;
            let fingerprint: Vec<u8> = prior
                .try_get("request_fingerprint")
                .map_err(|_| MigrationError::CommitFailed)?;
            if fingerprint.as_slice() != request_fingerprint {
                return Err(MigrationError::CheckpointConflict);
            }
            let receipt = decode_disposition_receipt(prior, true)?;
            tx.commit()
                .await
                .map_err(|_| MigrationError::CommitFailed)?;
            return Ok(receipt);
        };

        for correction in &request.corrections {
            let line = i64::try_from(correction.input_line)
                .map_err(|_| MigrationError::InvalidDisposition)?;
            sqlx::query("INSERT INTO litecr.migration_quarantine_disposition_item (disposition_id,input_line,duplicate_of_line,quarantine_code,original_payload_sha256,corrected_payload_sha256) VALUES ($1::uuid,$2,$3,$4,$5,$6)")
                .bind(&disposition_id)
                .bind(line)
                .bind(correction.duplicate_of_line.map(|value| i64::try_from(value).map_err(|_| MigrationError::InvalidDisposition)).transpose()?)
                .bind(&correction.quarantine_code)
                .bind(correction.original_payload_sha256.to_vec())
                .bind(correction.corrected_payload_sha256.map(|digest| digest.to_vec()))
                .execute(&mut *tx)
                .await
                .map_err(|_| MigrationError::CommitFailed)?;
        }

        sqlx::query("INSERT INTO litecr.mutation_audit (actor_kind,actor_issuer_url,actor_id,registry_id,organization_id,action,resource_type,resource_ref,outcome,reason_code,request_id,correlation_id,resource_version) VALUES ($1,$2,$3,$4,$5,'migration.quarantine.disposition','migration_quarantine_disposition',$6,'accepted',$7,$8,$9,1)")
            .bind(actor_kind).bind(issuer).bind(&actor.id).bind(&request.registry_id).bind(&request.organization_id).bind(&disposition_id).bind(action).bind(context.request_id()).bind(context.correlation_id())
            .execute(&mut *tx).await.map_err(|_| MigrationError::CommitFailed)?;
        let replacement = request
            .replacement_manifest_sha256
            .map(|digest| hex_digest(&digest));
        sqlx::query("INSERT INTO litecr.outbox_event (event_type,schema_version,registry_id,organization_id,aggregate_type,aggregate_id,aggregate_version,actor_kind,actor_id,correlation_id,payload) VALUES ('migration.quarantine.disposition',1,$1,$2,'migration_quarantine_disposition',$3::uuid,1,$4,$5,$6,$7)")
            .bind(&request.registry_id).bind(&request.organization_id).bind(&disposition_id).bind(actor_kind).bind(&actor.id).bind(context.correlation_id())
            .bind(json!({"action": action, "parent_manifest_sha256": hex_digest(&request.parent_manifest_sha256), "replacement_manifest_sha256": replacement, "correction_count": request.corrections.len()}))
            .execute(&mut *tx).await.map_err(|_| MigrationError::CommitFailed)?;
        tx.commit()
            .await
            .map_err(|_| MigrationError::CommitFailed)?;

        Ok(QuarantineDispositionReceipt {
            disposition_id,
            action: request.action,
            replacement_manifest_sha256: request.replacement_manifest_sha256,
            correction_count: request.corrections.len() as u64,
            replayed: false,
        })
    }

    async fn validate_batch(
        &self,
        context: &AccessContext,
        batch: &MigrationBatch,
    ) -> Result<(), MigrationError> {
        litecr_application::migration::validate_batch(batch)?;
        let scope = ResourceScope {
            registry_id: batch.run.registry_id.clone(),
            organization_id: batch.run.organization_id.clone(),
            record_id: None,
        };
        authorize(Some(context), Permission::MigrationRead, &scope)?;

        for record in &batch.records {
            map_supported_record(&batch.run, record)
                .map_err(|_| MigrationError::UnsupportedRecord)?;
        }
        Ok(())
    }

    async fn commit_batch_atomically(
        &self,
        context: &AccessContext,
        batch: &MigrationBatch,
    ) -> Result<BatchCommit, MigrationError> {
        litecr_application::migration::validate_batch(batch)?;
        let scope = ResourceScope {
            registry_id: batch.run.registry_id.clone(),
            organization_id: batch.run.organization_id.clone(),
            record_id: None,
        };
        authorize(Some(context), Permission::MigrationWrite, &scope)?;

        // Map the whole batch before opening a transaction. A single unsupported
        // field must not leave earlier records committed or partially applied.
        let mapped = batch
            .records
            .iter()
            .map(|record| {
                map_supported_record(&batch.run, record)
                    .map_err(|_| MigrationError::UnsupportedRecord)
            })
            .collect::<Result<Vec<_>, _>>()?;

        let actor = registry_actor(context);
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|_| MigrationError::CommitFailed)?;
        let checkpoint =
            PostgresMigrationJournal::establish_in_transaction(&mut tx, &batch.run, &actor.id)
                .await?;
        if checkpoint.state == MigrationRunState::Reversed {
            return Err(MigrationError::CheckpointConflict);
        }
        if batch.sequence <= checkpoint.committed_batch_sequence {
            let replay = PostgresMigrationJournal::record_batch_in_transaction(
                &mut tx,
                batch,
                &actor.id,
                &[],
            )
            .await?;
            tx.commit()
                .await
                .map_err(|_| MigrationError::CommitFailed)?;
            return Ok(replay);
        }
        if batch.sequence != checkpoint.committed_batch_sequence + 1 {
            return Err(MigrationError::CheckpointConflict);
        }

        let mut applied = Vec::with_capacity(batch.records.len());
        for (record, mapped_row) in batch.records.iter().zip(mapped.iter()) {
            applied.push(import_one(&mut tx, &actor, &batch.run, record, mapped_row).await?);
        }
        let commit = PostgresMigrationJournal::record_batch_in_transaction(
            &mut tx, batch, &actor.id, &applied,
        )
        .await?;
        tx.commit()
            .await
            .map_err(|_| MigrationError::CommitFailed)?;
        Ok(commit)
    }

    async fn checkpoint(
        &self,
        context: &AccessContext,
        run: &MigrationRun,
    ) -> Result<Option<MigrationCheckpoint>, MigrationError> {
        let scope = ResourceScope {
            registry_id: run.registry_id.clone(),
            organization_id: run.organization_id.clone(),
            record_id: None,
        };
        authorize(Some(context), Permission::MigrationRead, &scope)?;
        self.journal.checkpoint(run).await
    }

    async fn reverse_manifest_atomically(
        &self,
        context: &AccessContext,
        request: &ReverseManifest,
    ) -> Result<ReverseReceipt, MigrationError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|_| MigrationError::CommitFailed)?;
        let manifest = request.manifest_sha256.to_vec();
        let checkpoint = sqlx::query("SELECT registry_id,organization_id,expected_record_count,committed_batch_sequence,committed_record_count,state FROM litecr.migration_checkpoint WHERE manifest_sha256=$1 FOR UPDATE")
            .bind(&manifest)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|_| MigrationError::CheckpointConflict)?
            .ok_or(MigrationError::CheckpointConflict)?;
        let registry_id: String = checkpoint
            .try_get("registry_id")
            .map_err(|_| MigrationError::CheckpointConflict)?;
        let organization_id: String = checkpoint
            .try_get("organization_id")
            .map_err(|_| MigrationError::CheckpointConflict)?;
        if registry_id != request.registry_id || organization_id != request.organization_id {
            return Err(MigrationError::CheckpointConflict);
        }
        authorize(
            Some(context),
            Permission::MigrationWrite,
            &ResourceScope {
                registry_id: registry_id.clone(),
                organization_id: organization_id.clone(),
                record_id: None,
            },
        )?;

        if let Some(row) = sqlx::query("SELECT withdrawn_record_count,detached_identifier_count,retained_alias_count FROM litecr.migration_reverse_receipt WHERE manifest_sha256=$1")
            .bind(&manifest)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|_| MigrationError::CommitFailed)?
        {
            let receipt = ReverseReceipt {
                manifest_sha256: request.manifest_sha256,
                withdrawn_record_count: row.try_get::<i64, _>("withdrawn_record_count").map_err(|_| MigrationError::CommitFailed)? as u64,
                detached_identifier_count: row.try_get::<i64, _>("detached_identifier_count").map_err(|_| MigrationError::CommitFailed)? as u64,
                retained_alias_count: row.try_get::<i64, _>("retained_alias_count").map_err(|_| MigrationError::CommitFailed)? as u64,
                state: MigrationRunState::Reversed,
            };
            tx.commit().await.map_err(|_| MigrationError::CommitFailed)?;
            return Ok(receipt);
        }

        let evaluation = evaluate_reverse(&mut tx, &manifest, &checkpoint, true).await?;
        if let Some(error) = evaluation.reverse_error {
            return Err(error);
        }
        if !evaluation.blockers.is_empty() {
            return Err(MigrationError::ReverseBlocked);
        }
        let journal = evaluation.journal;
        let source_people = evaluation.source_people;

        let identifier_keys = sqlx::query("SELECT DISTINCT pi.registry_id,pi.authority,pi.identifier_type,pi.identifier_value FROM litecr.migration_effect e JOIN litecr.person_identifier pi ON pi.identifier_id=e.effect_ref WHERE e.manifest_sha256=$1 AND e.effect_kind='person_identifier' AND e.disposition='owned' ORDER BY pi.registry_id,pi.authority,pi.identifier_type,pi.identifier_value")
            .bind(&manifest).fetch_all(&mut *tx).await.map_err(|_| MigrationError::CommitFailed)?;
        for key in identifier_keys {
            let registry: String = key
                .try_get("registry_id")
                .map_err(|_| MigrationError::CommitFailed)?;
            let authority: String = key
                .try_get("authority")
                .map_err(|_| MigrationError::CommitFailed)?;
            let identifier_type: String = key
                .try_get("identifier_type")
                .map_err(|_| MigrationError::CommitFailed)?;
            let value: String = key
                .try_get("identifier_value")
                .map_err(|_| MigrationError::CommitFailed)?;
            let lock_material = format!(
                "{}:{}:{}:{}:{}:{}:{}:{}",
                registry.len(),
                registry,
                authority.len(),
                authority,
                identifier_type.len(),
                identifier_type,
                value.len(),
                value
            );
            sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
                .bind(lock_material)
                .execute(&mut *tx)
                .await
                .map_err(|_| MigrationError::CommitFailed)?;
        }

        // This is the first mutation: every source, latest version and identity
        // dependency across the full manifest has passed precheck.
        sqlx::query("UPDATE litecr.source_record_version v SET withdrawn_at=now(),withdrawn_by_manifest=$1 FROM litecr.migration_journal j WHERE j.manifest_sha256=$1 AND v.source_record_id=j.imported_source_record_id AND v.receive_version=j.imported_receive_version AND v.withdrawn_at IS NULL")
            .bind(&manifest).execute(&mut *tx).await.map_err(|_| MigrationError::CommitFailed)?;
        sqlx::query("UPDATE litecr.source_assertion a SET withdrawn_at=now(),withdrawn_by_manifest=$1 FROM litecr.migration_journal j WHERE j.manifest_sha256=$1 AND a.source_record_id=j.imported_source_record_id AND a.receive_version=j.imported_receive_version AND a.withdrawn_at IS NULL")
            .bind(&manifest).execute(&mut *tx).await.map_err(|_| MigrationError::CommitFailed)?;
        sqlx::query("UPDATE litecr.source_identifier_assertion a SET withdrawn_at=now(),withdrawn_by_manifest=$1 FROM litecr.migration_journal j WHERE j.manifest_sha256=$1 AND a.source_record_id=j.imported_source_record_id AND a.receive_version=j.imported_receive_version AND a.withdrawn_at IS NULL")
            .bind(&manifest).execute(&mut *tx).await.map_err(|_| MigrationError::CommitFailed)?;

        for (source_id, _person_id, _current_version) in &source_people {
            let baseline = journal
                .iter()
                .filter(|row| {
                    row.try_get::<String, _>("source_record_id").ok().as_deref()
                        == Some(source_id.as_str())
                })
                .filter_map(|row| {
                    row.try_get::<Option<i64>, _>("previous_receive_version")
                        .ok()
                        .flatten()
                })
                .min();
            let created = journal.iter().any(|row| {
                row.try_get::<String, _>("source_record_id").ok().as_deref()
                    == Some(source_id.as_str())
                    && row
                        .try_get::<bool, _>("source_record_created")
                        .unwrap_or(false)
            });
            // A source created by this manifest has no pre-import current
            // version to restore, even if later manifest rows recorded an
            // earlier imported version as their immediate predecessor.
            if created {
                sqlx::query("UPDATE litecr.source_record SET withdrawn_at=now(),withdrawn_by_manifest=$2,updated_at=now() WHERE source_record_id=$1::uuid")
                    .bind(source_id).bind(&manifest).execute(&mut *tx).await.map_err(|_| MigrationError::CommitFailed)?;
            } else if let Some(previous) = baseline {
                sqlx::query("UPDATE litecr.source_record SET current_receive_version=$2,updated_at=now() WHERE source_record_id=$1::uuid")
                    .bind(source_id).bind(previous).execute(&mut *tx).await.map_err(|_| MigrationError::CommitFailed)?;
            } else {
                return Err(MigrationError::ReverseBlocked);
            }
        }

        let identifier_effects = sqlx::query("SELECT e.effect_id::text AS effect_id,e.effect_ref::text AS identifier_id,e.source_record_id::text AS source_record_id,pi.source_record_id::text AS owner_source_record_id,pi.person_id::text AS identifier_person_id,sr.person_id::text AS imported_person_id,pi.registry_id,pi.organization_id,pi.authority,pi.identifier_type,pi.identifier_value FROM litecr.migration_effect e JOIN litecr.person_identifier pi ON pi.identifier_id=e.effect_ref JOIN litecr.source_record sr ON sr.source_record_id=e.source_record_id WHERE e.manifest_sha256=$1 AND e.effect_kind='person_identifier' AND e.disposition='owned' ORDER BY e.effect_ref FOR UPDATE OF pi")
            .bind(&manifest).fetch_all(&mut *tx).await.map_err(|_| MigrationError::CommitFailed)?;
        let mut detached_identifiers = 0_u64;
        for effect in identifier_effects {
            let effect_id: String = effect
                .try_get("effect_id")
                .map_err(|_| MigrationError::CommitFailed)?;
            let identifier_id: String = effect
                .try_get("identifier_id")
                .map_err(|_| MigrationError::CommitFailed)?;
            let source_record_id: String = effect
                .try_get("source_record_id")
                .map_err(|_| MigrationError::CommitFailed)?;
            let registry: String = effect
                .try_get("registry_id")
                .map_err(|_| MigrationError::CommitFailed)?;
            let authority: String = effect
                .try_get("authority")
                .map_err(|_| MigrationError::CommitFailed)?;
            let identifier_type: String = effect
                .try_get("identifier_type")
                .map_err(|_| MigrationError::CommitFailed)?;
            let value: String = effect
                .try_get("identifier_value")
                .map_err(|_| MigrationError::CommitFailed)?;
            let owner_source_record_id: String = effect
                .try_get("owner_source_record_id")
                .map_err(|_| MigrationError::CommitFailed)?;
            let identifier_person_id: String = effect
                .try_get("identifier_person_id")
                .map_err(|_| MigrationError::CommitFailed)?;
            let imported_person_id: String = effect
                .try_get("imported_person_id")
                .map_err(|_| MigrationError::CommitFailed)?;
            let later_consumer: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM litecr.source_identifier_assertion a WHERE a.registry_id=$1 AND a.authority=$2 AND a.identifier_type=$3 AND a.original_value=$4 AND NOT EXISTS (SELECT 1 FROM litecr.migration_journal j WHERE j.manifest_sha256=$5 AND j.imported_source_record_id=a.source_record_id AND j.imported_receive_version=a.receive_version))")
                .bind(&registry).bind(&authority).bind(&identifier_type).bind(&value).bind(&manifest)
                .fetch_one(&mut *tx).await.map_err(|_| MigrationError::CommitFailed)?;
            if source_record_id != owner_source_record_id
                || identifier_person_id != imported_person_id
                || later_consumer
            {
                sqlx::query("UPDATE litecr.migration_effect SET disposition='retained',resolution_code='identifier_has_other_source_assertion' WHERE effect_id=$1::uuid")
                    .bind(effect_id).execute(&mut *tx).await.map_err(|_| MigrationError::CommitFailed)?;
            } else {
                sqlx::query("DELETE FROM litecr.person_identifier WHERE identifier_id=$1::uuid")
                    .bind(identifier_id)
                    .execute(&mut *tx)
                    .await
                    .map_err(|_| MigrationError::CommitFailed)?;
                sqlx::query("UPDATE litecr.migration_effect SET disposition='detached',resolution_code='no_other_source_assertion' WHERE effect_id=$1::uuid")
                    .bind(effect_id).execute(&mut *tx).await.map_err(|_| MigrationError::CommitFailed)?;
                detached_identifiers += 1;
            }
        }
        let retained_aliases: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM litecr.migration_effect WHERE manifest_sha256=$1 AND effect_kind='person_alias' AND disposition='owned'")
            .bind(&manifest).fetch_one(&mut *tx).await.map_err(|_| MigrationError::CommitFailed)?;
        sqlx::query("UPDATE litecr.migration_effect SET disposition='retained',resolution_code='alias_consumer_usage_not_persisted' WHERE manifest_sha256=$1 AND effect_kind='person_alias' AND disposition='owned'")
            .bind(&manifest).execute(&mut *tx).await.map_err(|_| MigrationError::CommitFailed)?;

        // Withdrawing imported source assertions changes the projected Patient
        // representation even when an older source version becomes current.
        // Keep the public resource version monotonic for every affected person.
        let affected_person_ids = source_people
            .iter()
            .map(|(_, person_id, _)| person_id.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        for person_id in affected_person_ids {
            let updated = sqlx::query(
                "UPDATE litecr.person SET resource_version=resource_version+1 WHERE person_id=$1::uuid",
            )
            .bind(person_id)
            .execute(&mut *tx)
            .await
            .map_err(|_| MigrationError::CommitFailed)?;
            if updated.rows_affected() != 1 {
                return Err(MigrationError::ReverseBlocked);
            }
        }

        let withdrawn_count = source_people.len() as u64;
        for (source_id, _person_id, current_version) in &source_people {
            sqlx::query("UPDATE litecr.migration_journal SET reverse_state='reversed',reversed_at=now(),reverse_refusal_code=NULL WHERE manifest_sha256=$1 AND imported_source_record_id=$2::uuid")
                .bind(&manifest).bind(source_id).execute(&mut *tx).await.map_err(|_| MigrationError::CommitFailed)?;
            sqlx::query("INSERT INTO litecr.mutation_audit (actor_kind,actor_issuer_url,actor_id,registry_id,organization_id,action,resource_type,resource_ref,outcome,reason_code,request_id,correlation_id,resource_version) VALUES ($1,$2,$3,$4,$5,'migration.source.withdraw','source_record',$6,'accepted','MIGRATION_REVERSED',$7,$8,$9)")
                .bind(actor_kind_name(context.actor().kind)).bind(context.actor().issuer_url.as_deref().unwrap_or_default()).bind(&context.actor().id).bind(&registry_id).bind(&organization_id).bind(source_id).bind(context.request_id()).bind(context.correlation_id()).bind(current_version)
                .execute(&mut *tx).await.map_err(|_| MigrationError::CommitFailed)?;
            sqlx::query("INSERT INTO litecr.outbox_event (event_type,schema_version,registry_id,organization_id,aggregate_type,aggregate_id,aggregate_version,actor_kind,actor_id,correlation_id,payload) VALUES ('registry.source_record.withdrawn',1,$1,$2,'source_record',$3::uuid,$4,$5,$6,$7,$8)")
                .bind(&registry_id).bind(&organization_id).bind(source_id).bind(current_version).bind(actor_kind_name(context.actor().kind)).bind(&context.actor().id).bind(context.correlation_id())
                .bind(json!({"source_record_id":source_id,"withdrawn_receive_versions":journal.iter().filter(|row|row.try_get::<String,_>("source_record_id").ok().as_deref()==Some(source_id.as_str())).filter_map(|row|row.try_get::<i64,_>("imported_receive_version").ok()).collect::<Vec<_>>(),"migration_manifest_sha256":hex(&request.manifest_sha256),"reason":request.reason}))
                .execute(&mut *tx).await.map_err(|_| MigrationError::CommitFailed)?;
        }
        let receipt = ReverseReceipt {
            manifest_sha256: request.manifest_sha256,
            withdrawn_record_count: withdrawn_count,
            detached_identifier_count: detached_identifiers,
            retained_alias_count: retained_aliases.max(0) as u64,
            state: MigrationRunState::Reversed,
        };
        sqlx::query("INSERT INTO litecr.migration_reverse_receipt (manifest_sha256,registry_id,organization_id,reversed_by,reason,withdrawn_record_count,detached_identifier_count,retained_alias_count) VALUES ($1,$2,$3,$4,$5,$6,$7,$8)")
            .bind(&manifest).bind(&registry_id).bind(&organization_id).bind(&context.actor().id).bind(&request.reason).bind(receipt.withdrawn_record_count as i64).bind(receipt.detached_identifier_count as i64).bind(receipt.retained_alias_count as i64)
            .execute(&mut *tx).await.map_err(|_| MigrationError::CommitFailed)?;
        sqlx::query("UPDATE litecr.migration_checkpoint SET state='reversed',updated_at=now() WHERE manifest_sha256=$1")
            .bind(&manifest).execute(&mut *tx).await.map_err(|_| MigrationError::CommitFailed)?;
        tx.commit()
            .await
            .map_err(|_| MigrationError::CommitFailed)?;
        Ok(receipt)
    }

    async fn reverse_preflight(
        &self,
        context: &AccessContext,
        request: &ReversePreflightRequest,
    ) -> Result<ReversePreflight, MigrationError> {
        authorize(
            Some(context),
            Permission::MigrationRead,
            &ResourceScope {
                registry_id: request.registry_id.clone(),
                organization_id: request.organization_id.clone(),
                record_id: None,
            },
        )?;
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|_| MigrationError::CommitFailed)?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
            .execute(&mut *tx)
            .await
            .map_err(|_| MigrationError::CommitFailed)?;
        let manifest = request.manifest_sha256.to_vec();
        let Some(checkpoint) = sqlx::query("SELECT registry_id,organization_id,expected_record_count,committed_batch_sequence,committed_record_count,state FROM litecr.migration_checkpoint WHERE manifest_sha256=$1")
            .bind(&manifest).fetch_optional(&mut *tx).await.map_err(|_| MigrationError::CheckpointConflict)? else {
                tx.commit().await.map_err(|_| MigrationError::CommitFailed)?;
                return Ok(ReversePreflight { eligible: false, blockers: vec![ReverseBlockerCount { code: ReverseBlockerCode::InvalidCheckpoint, count: 1 }] });
            };
        let registry_id: String = checkpoint
            .try_get("registry_id")
            .map_err(|_| MigrationError::CheckpointConflict)?;
        let organization_id: String = checkpoint
            .try_get("organization_id")
            .map_err(|_| MigrationError::CheckpointConflict)?;
        if registry_id != request.registry_id || organization_id != request.organization_id {
            tx.commit()
                .await
                .map_err(|_| MigrationError::CommitFailed)?;
            return Ok(ReversePreflight {
                eligible: false,
                blockers: vec![ReverseBlockerCount {
                    code: ReverseBlockerCode::InvalidCheckpoint,
                    count: 1,
                }],
            });
        }
        if registry_id != request.registry_id || organization_id != request.organization_id {
            return Err(MigrationError::CheckpointConflict);
        }
        let already_reversed: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM litecr.migration_reverse_receipt WHERE manifest_sha256=$1)")
            .bind(&manifest).fetch_one(&mut *tx).await.map_err(|_| MigrationError::CommitFailed)?;
        if already_reversed {
            tx.commit()
                .await
                .map_err(|_| MigrationError::CommitFailed)?;
            return Ok(ReversePreflight {
                eligible: false,
                blockers: vec![ReverseBlockerCount {
                    code: ReverseBlockerCode::AlreadyReversed,
                    count: 1,
                }],
            });
        }
        let evaluation = evaluate_reverse(&mut tx, &manifest, &checkpoint, false).await?;
        tx.commit()
            .await
            .map_err(|_| MigrationError::CommitFailed)?;
        let blockers = evaluation
            .blockers
            .into_iter()
            .map(|(code, count)| ReverseBlockerCount { code, count })
            .collect::<Vec<_>>();
        Ok(ReversePreflight {
            eligible: blockers.is_empty(),
            blockers,
        })
    }
}

struct ReverseEvaluation {
    journal: Vec<sqlx::postgres::PgRow>,
    source_people: Vec<(String, String, i64)>,
    blockers: BTreeMap<ReverseBlockerCode, u64>,
    reverse_error: Option<MigrationError>,
}

fn add_blocker(blockers: &mut BTreeMap<ReverseBlockerCode, u64>, code: ReverseBlockerCode) {
    *blockers.entry(code).or_default() += 1;
}

/// The shared, authoritative reverse eligibility evaluator. Reverse invokes it
/// under row/advisory locks; preflight invokes the same SQL/checks in a
/// repeatable-read, read-only snapshot. Only reverse may mutate, and it always
/// evaluates again while holding locks.
async fn evaluate_reverse(
    tx: &mut sqlx::Transaction<'_, Postgres>,
    manifest: &[u8],
    checkpoint: &sqlx::postgres::PgRow,
    lock_rows: bool,
) -> Result<ReverseEvaluation, MigrationError> {
    let mut blockers = BTreeMap::new();
    let expected_count: i64 = checkpoint
        .try_get("expected_record_count")
        .map_err(|_| MigrationError::CheckpointConflict)?;
    let committed_count: i64 = checkpoint
        .try_get("committed_record_count")
        .map_err(|_| MigrationError::CheckpointConflict)?;
    let committed_batch_sequence: i64 = checkpoint
        .try_get("committed_batch_sequence")
        .map_err(|_| MigrationError::CheckpointConflict)?;
    let state: String = checkpoint
        .try_get("state")
        .map_err(|_| MigrationError::CheckpointConflict)?;
    let journal_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM litecr.migration_journal WHERE manifest_sha256=$1",
    )
    .bind(manifest)
    .fetch_one(&mut **tx)
    .await
    .map_err(|_| MigrationError::CommitFailed)?;
    let fully_committed = state == "completed" && expected_count == committed_count;
    let partially_committed =
        state == "importing" && committed_count > 0 && expected_count > committed_count;
    let batch_stats = sqlx::query(
        "SELECT COUNT(*) AS batch_count,COALESCE(SUM(record_count),0) AS batch_record_count,COALESCE(MAX(batch_sequence),0) AS max_batch_sequence FROM litecr.migration_batch WHERE manifest_sha256=$1",
    )
    .bind(manifest)
    .fetch_one(&mut **tx)
    .await
    .map_err(|_| MigrationError::CommitFailed)?;
    let batch_count: i64 = batch_stats
        .try_get("batch_count")
        .map_err(|_| MigrationError::CheckpointConflict)?;
    let batch_record_count: i64 = batch_stats
        .try_get("batch_record_count")
        .map_err(|_| MigrationError::CheckpointConflict)?;
    let max_batch_sequence: i64 = batch_stats
        .try_get("max_batch_sequence")
        .map_err(|_| MigrationError::CheckpointConflict)?;
    if (!fully_committed && !partially_committed)
        || committed_count <= 0
        || journal_count != committed_count
        || batch_record_count != committed_count
        || batch_count != committed_batch_sequence
        || max_batch_sequence != committed_batch_sequence
    {
        add_blocker(&mut blockers, ReverseBlockerCode::InvalidCheckpoint);
        return Ok(ReverseEvaluation {
            journal: Vec::new(),
            source_people: Vec::new(),
            blockers,
            reverse_error: Some(MigrationError::CheckpointConflict),
        });
    }

    let journal_sql = "SELECT journal_id::text AS journal_id,imported_source_record_id::text AS source_record_id,imported_receive_version,expected_current_receive_version,expected_current_payload_sha256,source_record_created,previous_receive_version,reverse_state FROM litecr.migration_journal WHERE manifest_sha256=$1 ORDER BY imported_source_record_id,imported_receive_version";
    let journal = if lock_rows {
        sqlx::query(&format!("{journal_sql} FOR UPDATE"))
            .bind(manifest)
            .fetch_all(&mut **tx)
            .await
            .map_err(|_| MigrationError::CommitFailed)?
    } else {
        sqlx::query(journal_sql)
            .bind(manifest)
            .fetch_all(&mut **tx)
            .await
            .map_err(|_| MigrationError::CommitFailed)?
    };
    let journal_mismatch = journal.len() as i64 != journal_count
        || journal.iter().any(|row| {
            row.try_get::<String, _>("reverse_state").ok().as_deref() != Some("pending")
        });
    if journal_mismatch {
        add_blocker(&mut blockers, ReverseBlockerCode::JournalMismatch);
    }
    let intact_imported_versions: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM litecr.migration_journal j JOIN litecr.source_record_version v ON v.source_record_id=j.imported_source_record_id AND v.receive_version=j.imported_receive_version WHERE j.manifest_sha256=$1 AND v.withdrawn_at IS NULL")
        .bind(manifest).fetch_one(&mut **tx).await.map_err(|_| MigrationError::CommitFailed)?;
    if intact_imported_versions != journal_count {
        add_blocker(&mut blockers, ReverseBlockerCode::JournalMismatch);
    }
    if journal_mismatch {
        return Ok(ReverseEvaluation {
            journal,
            source_people: Vec::new(),
            blockers,
            reverse_error: Some(MigrationError::CheckpointConflict),
        });
    }
    if !blockers.is_empty() {
        return Ok(ReverseEvaluation {
            journal,
            source_people: Vec::new(),
            blockers,
            reverse_error: Some(MigrationError::ReverseBlocked),
        });
    }

    if lock_rows {
        let source_keys = sqlx::query("SELECT DISTINCT sr.registry_id,sr.organization_id,sr.source_system_id,sr.source_record_key FROM litecr.source_record sr JOIN litecr.migration_journal j ON j.imported_source_record_id=sr.source_record_id WHERE j.manifest_sha256=$1 ORDER BY sr.registry_id,sr.organization_id,sr.source_system_id,sr.source_record_key")
            .bind(manifest).fetch_all(&mut **tx).await.map_err(|_| MigrationError::CommitFailed)?;
        for key in source_keys {
            let registry: String = key
                .try_get("registry_id")
                .map_err(|_| MigrationError::CommitFailed)?;
            let organization: String = key
                .try_get("organization_id")
                .map_err(|_| MigrationError::CommitFailed)?;
            let system: String = key
                .try_get("source_system_id")
                .map_err(|_| MigrationError::CommitFailed)?;
            let record: String = key
                .try_get("source_record_key")
                .map_err(|_| MigrationError::CommitFailed)?;
            let lock_material = format!(
                "{}:{}:{}:{}:{}:{}:{}:{}",
                registry.len(),
                registry,
                organization.len(),
                organization,
                system.len(),
                system,
                record.len(),
                record
            );
            sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
                .bind(lock_material)
                .execute(&mut **tx)
                .await
                .map_err(|_| MigrationError::CommitFailed)?;
        }
    }

    let sources_sql = "SELECT sr.source_record_id::text AS source_record_id,sr.person_id::text AS person_id,sr.current_receive_version,(sr.withdrawn_at IS NOT NULL) AS is_withdrawn FROM litecr.source_record sr WHERE sr.source_record_id IN (SELECT DISTINCT imported_source_record_id FROM litecr.migration_journal WHERE manifest_sha256=$1) ORDER BY sr.source_record_id";
    let sources = if lock_rows {
        sqlx::query(&format!("{sources_sql} FOR UPDATE"))
            .bind(manifest)
            .fetch_all(&mut **tx)
            .await
            .map_err(|_| MigrationError::CommitFailed)?
    } else {
        sqlx::query(sources_sql)
            .bind(manifest)
            .fetch_all(&mut **tx)
            .await
            .map_err(|_| MigrationError::CommitFailed)?
    };
    let distinct_journal_sources = journal
        .iter()
        .filter_map(|row| row.try_get::<String, _>("source_record_id").ok())
        .collect::<std::collections::BTreeSet<_>>();
    if sources.len() < distinct_journal_sources.len() {
        *blockers
            .entry(ReverseBlockerCode::MissingCurrentSource)
            .or_default() += (distinct_journal_sources.len() - sources.len()) as u64;
    }
    let mut source_people = Vec::with_capacity(sources.len());
    for source in &sources {
        let source_id: String = source
            .try_get("source_record_id")
            .map_err(|_| MigrationError::CommitFailed)?;
        let person_id: String = source
            .try_get("person_id")
            .map_err(|_| MigrationError::CommitFailed)?;
        let current_version: i64 = source
            .try_get("current_receive_version")
            .map_err(|_| MigrationError::CommitFailed)?;
        if source
            .try_get::<bool, _>("is_withdrawn")
            .map_err(|_| MigrationError::CommitFailed)?
        {
            add_blocker(&mut blockers, ReverseBlockerCode::SourceAlreadyWithdrawn);
            continue;
        }
        let imported = journal
            .iter()
            .filter(|row| {
                row.try_get::<String, _>("source_record_id").ok().as_deref()
                    == Some(source_id.as_str())
            })
            .collect::<Vec<_>>();
        let Some(max_imported) = imported
            .iter()
            .filter_map(|row| row.try_get::<i64, _>("imported_receive_version").ok())
            .max()
        else {
            add_blocker(&mut blockers, ReverseBlockerCode::JournalMismatch);
            continue;
        };
        let Some(expected_current) = imported
            .iter()
            .filter_map(|row| {
                row.try_get::<i64, _>("expected_current_receive_version")
                    .ok()
            })
            .max()
        else {
            add_blocker(&mut blockers, ReverseBlockerCode::JournalMismatch);
            continue;
        };
        let mut source_blockers = std::collections::BTreeSet::new();
        if current_version != expected_current {
            source_blockers.insert(ReverseBlockerCode::SourceChanged);
        }
        let expected_hash = imported
            .iter()
            .find(|row| {
                row.try_get::<i64, _>("expected_current_receive_version")
                    .ok()
                    == Some(expected_current)
            })
            .and_then(|row| {
                row.try_get::<Vec<u8>, _>("expected_current_payload_sha256")
                    .ok()
            });
        if expected_hash.is_none() {
            source_blockers.insert(ReverseBlockerCode::JournalMismatch);
        }
        let current_hash = sqlx::query_scalar::<_, Vec<u8>>("SELECT payload_hash FROM litecr.source_record_version WHERE source_record_id=$1::uuid AND receive_version=$2 AND withdrawn_at IS NULL")
            .bind(&source_id).bind(current_version).fetch_optional(&mut **tx).await.map_err(|_| MigrationError::CommitFailed)?;
        if expected_hash.as_ref() != current_hash.as_ref() {
            source_blockers.insert(ReverseBlockerCode::SourceChanged);
        }
        let later_version: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM litecr.source_record_version WHERE source_record_id=$1::uuid AND receive_version>$2)")
            .bind(&source_id).bind(max_imported).fetch_one(&mut **tx).await.map_err(|_| MigrationError::CommitFailed)?;
        if later_version {
            source_blockers.insert(ReverseBlockerCode::LaterSourceVersion);
        }
        let has_identity_dependency: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM litecr.review_case WHERE query_source_record_id=$1::uuid OR candidate_source_record_id=$1::uuid) OR EXISTS(SELECT 1 FROM litecr.identity_merge_member WHERE source_record_id=$1::uuid) OR EXISTS(SELECT 1 FROM litecr.confirmed_non_match WHERE left_source_record_id=$1::uuid OR right_source_record_id=$1::uuid) OR EXISTS(SELECT 1 FROM litecr.identity_link WHERE source_person_id=$2::uuid OR target_person_id=$2::uuid) OR EXISTS(SELECT 1 FROM litecr.identity_merge WHERE source_person_id=$2::uuid OR target_person_id=$2::uuid) OR EXISTS(SELECT 1 FROM litecr.person_redirect WHERE source_person_id=$2::uuid OR target_person_id=$2::uuid)")
            .bind(&source_id).bind(&person_id).fetch_one(&mut **tx).await.map_err(|_| MigrationError::CommitFailed)?;
        if has_identity_dependency {
            source_blockers.insert(ReverseBlockerCode::IdentityDependency);
        }
        for code in &source_blockers {
            add_blocker(&mut blockers, *code);
        }
        if source_blockers.is_empty() {
            source_people.push((source_id, person_id, current_version));
        }
    }
    let person_ids = source_people
        .iter()
        .map(|(_, person_id, _)| person_id.clone())
        .collect::<std::collections::BTreeSet<_>>();
    for person_id in person_ids {
        let person_sql = if lock_rows {
            "SELECT person_id FROM litecr.person WHERE person_id=$1::uuid FOR UPDATE"
        } else {
            "SELECT person_id FROM litecr.person WHERE person_id=$1::uuid"
        };
        if sqlx::query(person_sql)
            .bind(person_id)
            .fetch_optional(&mut **tx)
            .await
            .map_err(|_| MigrationError::CommitFailed)?
            .is_none()
        {
            add_blocker(&mut blockers, ReverseBlockerCode::MissingPerson);
        }
    }
    let reverse_error = (!blockers.is_empty()).then_some(MigrationError::ReverseBlocked);
    Ok(ReverseEvaluation {
        journal,
        source_people,
        blockers,
        reverse_error,
    })
}

fn registry_actor(context: &AccessContext) -> RegistryActor {
    RegistryActor {
        kind: context.actor().kind,
        id: context.actor().id.clone(),
        issuer_url: context.actor().issuer_url.clone(),
        request_id: context.request_id().to_owned(),
        correlation_id: context.correlation_id().map(str::to_owned),
    }
}

fn actor_kind_name(kind: ActorKind) -> &'static str {
    match kind {
        ActorKind::Human => "human",
        ActorKind::IntegrationClient => "integration_client",
        ActorKind::System => "system",
    }
}

fn disposition_action_name(action: QuarantineDispositionAction) -> &'static str {
    match action {
        QuarantineDispositionAction::CorrectedResubmission => "corrected_resubmission",
        QuarantineDispositionAction::Deferred => "deferred",
        QuarantineDispositionAction::DuplicateAcknowledged => "duplicate_acknowledged",
    }
}

fn hex_digest(digest: &[u8; 32]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut result = String::with_capacity(64);
    for byte in digest {
        result.push(HEX[(byte >> 4) as usize] as char);
        result.push(HEX[(byte & 0x0f) as usize] as char);
    }
    result
}

fn decode_disposition_receipt(
    row: sqlx::postgres::PgRow,
    replayed: bool,
) -> Result<QuarantineDispositionReceipt, MigrationError> {
    let action: String = row
        .try_get("action")
        .map_err(|_| MigrationError::CommitFailed)?;
    let replacement: Option<Vec<u8>> = row
        .try_get("replacement_manifest_sha256")
        .map_err(|_| MigrationError::CommitFailed)?;
    Ok(QuarantineDispositionReceipt {
        disposition_id: row
            .try_get("disposition_id")
            .map_err(|_| MigrationError::CommitFailed)?,
        action: match action.as_str() {
            "corrected_resubmission" => QuarantineDispositionAction::CorrectedResubmission,
            "deferred" => QuarantineDispositionAction::Deferred,
            "duplicate_acknowledged" => QuarantineDispositionAction::DuplicateAcknowledged,
            _ => return Err(MigrationError::CommitFailed),
        },
        replacement_manifest_sha256: replacement
            .map(|digest| digest.try_into().map_err(|_| MigrationError::CommitFailed))
            .transpose()?,
        correction_count: row
            .try_get::<i32, _>("correction_count")
            .map_err(|_| MigrationError::CommitFailed)? as u64,
        replayed,
    })
}

fn put_hash_field(hasher: &mut Sha256, value: Option<&str>) {
    match value {
        Some(value) => {
            hasher.update([1]);
            hasher.update((value.len() as u64).to_be_bytes());
            hasher.update(value.as_bytes());
        }
        None => hasher.update([0]),
    }
}

fn command_hash(command: &SourceRecordCommand) -> [u8; 32] {
    let mut hasher = Sha256::new();
    put_hash_field(&mut hasher, Some("litecr-api-v1/source.upsert"));
    for value in [
        Some(command.registry_id.as_str()),
        Some(command.organization_id.as_str()),
        Some(command.source_system_id.as_str()),
        Some(command.source_record_key.as_str()),
        command.source_version.as_deref(),
    ] {
        put_hash_field(&mut hasher, value);
    }
    hasher.update((command.assertions.len() as u64).to_be_bytes());
    for assertion in &command.assertions {
        put_hash_field(&mut hasher, Some(&assertion.field_path));
        put_hash_field(&mut hasher, Some(&assertion.original_value));
        put_hash_field(&mut hasher, assertion.language_tag.as_deref());
        put_hash_field(&mut hasher, assertion.script.as_deref());
    }
    hasher.update((command.identifiers.len() as u64).to_be_bytes());
    for identifier in &command.identifiers {
        put_hash_field(&mut hasher, Some(&identifier.authority));
        put_hash_field(&mut hasher, Some(&identifier.identifier_type));
        put_hash_field(&mut hasher, Some(&identifier.value));
        hasher.update([u8::from(identifier.authoritative)]);
    }
    hasher.finalize().into()
}

fn stable_migration_key(
    manifest_sha256: &[u8; 32],
    record: &litecr_application::migration::MigrationRecord,
) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"litecr-migration-idempotency-v1");
    hasher.update(manifest_sha256);
    for part in [
        record.source_system_id.as_str(),
        record.source_record_key.as_str(),
        record.source_version.as_str(),
    ] {
        hasher.update((part.len() as u64).to_be_bytes());
        hasher.update(part.as_bytes());
    }
    hasher.finalize().into()
}

async fn import_one(
    tx: &mut sqlx::Transaction<'_, Postgres>,
    actor: &RegistryActor,
    run: &MigrationRun,
    record: &litecr_application::migration::MigrationRecord,
    mapped: &MappedMigrationSource,
) -> Result<AppliedSource, MigrationError> {
    let command = &mapped.command;
    let actor_kind = actor_kind_name(actor.kind);
    let issuer = actor.issuer_url.as_deref().unwrap_or_default();
    let fingerprint = command_hash(command);
    let idempotency_hash = stable_migration_key(&run.manifest_sha256, record);
    let request_key = idempotency_hash.to_vec();

    // The source-key lock material matches the ordinary Registry writer.
    let source_lock = format!(
        "{}:{}:{}:{}:{}:{}:{}:{}",
        command.registry_id.len(),
        command.registry_id,
        command.organization_id.len(),
        command.organization_id,
        command.source_system_id.len(),
        command.source_system_id,
        command.source_record_key.len(),
        command.source_record_key,
    );
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
        .bind(source_lock)
        .execute(&mut **tx)
        .await
        .map_err(|_| MigrationError::CommitFailed)?;

    // The application-level source payload hash intentionally hashes Registry
    // assertions and identifiers only. The migration journal also binds IDs,
    // aliases, date precision, and the exact row bytes, so guard this wider
    // identity/version tuple before any mutation. Same-run retries have already
    // been returned by the durable batch checkpoint; reaching this branch with
    // a journal row means the batch does not agree and must fail closed.
    let prior_source_version = sqlx::query("SELECT j.source_payload_sha256,j.manifest_sha256,j.batch_sequence FROM litecr.migration_journal j JOIN litecr.migration_checkpoint c ON c.manifest_sha256=j.manifest_sha256 WHERE c.registry_id=$1 AND c.organization_id=$2 AND j.source_system_id=$3 AND j.source_record_key=$4 AND j.source_version=$5 ORDER BY j.imported_at DESC LIMIT 1 FOR UPDATE OF j")
        .bind(&command.registry_id)
        .bind(&command.organization_id)
        .bind(&record.source_system_id)
        .bind(&record.source_record_key)
        .bind(&record.source_version)
        .fetch_optional(&mut **tx)
        .await
        .map_err(|_| MigrationError::CommitFailed)?;
    if let Some(prior) = prior_source_version {
        let prior_digest: Vec<u8> = prior
            .try_get("source_payload_sha256")
            .map_err(|_| MigrationError::CommitFailed)?;
        if prior_digest.as_slice() != record.payload_sha256 {
            return Err(MigrationError::CheckpointConflict);
        }
        // Exact bytes are idempotent only when the same manifest and batch
        // journal entry is being replayed; this path is handled before import.
        return Err(MigrationError::CheckpointConflict);
    }

    let prior = sqlx::query("SELECT command_fingerprint FROM litecr.registry_idempotency WHERE actor_kind=$1 AND actor_issuer_url=$2 AND actor_id=$3 AND operation='source.upsert' AND registry_id=$4 AND idempotency_key_hash=$5 FOR UPDATE")
        .bind(actor_kind).bind(issuer).bind(&actor.id).bind(&command.registry_id).bind(&request_key)
        .fetch_optional(&mut **tx).await.map_err(|_|MigrationError::CommitFailed)?;
    if let Some(prior) = prior {
        let old: Vec<u8> = prior
            .try_get("command_fingerprint")
            .map_err(|_| MigrationError::CommitFailed)?;
        if old.as_slice() != fingerprint {
            return Err(MigrationError::CheckpointConflict);
        }
        // A committed migration retry is returned from the durable batch journal
        // before this function is called. A lone Registry idempotency row means
        // metadata is incomplete, so don't fabricate a journal receipt.
        return Err(MigrationError::CheckpointConflict);
    }

    // Resolve a source record in the exact requested scope; a key elsewhere is
    // never disclosed or adopted.
    let current = sqlx::query("SELECT sr.source_record_id::text AS source_record_id,sr.person_id::text AS person_id,sr.current_receive_version,(sr.withdrawn_at IS NOT NULL) AS source_withdrawn,p.public_id,sv.source_version,sv.payload_hash FROM litecr.source_record sr JOIN litecr.person p ON p.person_id=sr.person_id JOIN litecr.source_record_version sv ON sv.source_record_id=sr.source_record_id AND sv.receive_version=sr.current_receive_version WHERE sr.registry_id=$1 AND sr.organization_id=$2 AND sr.source_system_id=$3 AND sr.source_record_key=$4 FOR UPDATE OF sr,p")
        .bind(&command.registry_id).bind(&command.organization_id).bind(&command.source_system_id).bind(&command.source_record_key)
        .fetch_optional(&mut **tx).await.map_err(|_|MigrationError::CommitFailed)?;

    let (
        source_id,
        person_id,
        public_id,
        is_new_person,
        source_record_created,
        previous_receive_version,
    ) = if let Some(row) = &current {
        if row
            .try_get::<bool, _>("source_withdrawn")
            .map_err(|_| MigrationError::CommitFailed)?
        {
            return Err(MigrationError::CheckpointConflict);
        }
        let source_id: String = row
            .try_get("source_record_id")
            .map_err(|_| MigrationError::CommitFailed)?;
        let person_id: String = row
            .try_get("person_id")
            .map_err(|_| MigrationError::CommitFailed)?;
        let public_id: String = row
            .try_get("public_id")
            .map_err(|_| MigrationError::CommitFailed)?;
        if mapped
            .preserved_public_id
            .as_deref()
            .is_some_and(|given| given != public_id)
        {
            return Err(MigrationError::IdentityCollision);
        }
        let previous_receive_version = row
            .try_get::<i64, _>("current_receive_version")
            .map_err(|_| MigrationError::CommitFailed)?;
        (
            source_id,
            person_id,
            public_id,
            false,
            false,
            Some(previous_receive_version),
        )
    } else {
        let requested = mapped.preserved_public_id.as_deref();
        let existing = if let Some(public_id) = requested {
            sqlx::query("SELECT person_id::text AS person_id,registry_id,organization_id,public_id FROM litecr.person WHERE public_id=$1 FOR UPDATE")
                .bind(public_id).fetch_optional(&mut **tx).await.map_err(|_|MigrationError::CommitFailed)?
        } else {
            None
        };
        if let Some(row) = existing {
            let registry_id: String = row
                .try_get("registry_id")
                .map_err(|_| MigrationError::CommitFailed)?;
            let organization_id: String = row
                .try_get("organization_id")
                .map_err(|_| MigrationError::CommitFailed)?;
            if registry_id != command.registry_id || organization_id != command.organization_id {
                return Err(MigrationError::IdentityCollision);
            }
            let person_id: String = row
                .try_get("person_id")
                .map_err(|_| MigrationError::CommitFailed)?;
            let public_id: String = row
                .try_get("public_id")
                .map_err(|_| MigrationError::CommitFailed)?;
            let source_id = create_source_record(tx, command, &person_id).await?;
            (source_id, person_id, public_id, false, true, None)
        } else {
            let (person_id, public_id) = if let Some(public_id) = requested {
                let person_id = sqlx::query_scalar::<_, String>("INSERT INTO litecr.person (public_id,registry_id,organization_id) VALUES ($1,$2,$3) RETURNING person_id::text")
                    .bind(public_id).bind(&command.registry_id).bind(&command.organization_id)
                    .fetch_one(&mut **tx).await.map_err(|error| {
                        if is_unique_violation(&error) { MigrationError::IdentityCollision } else { MigrationError::CommitFailed }
                    })?;
                (person_id, public_id.to_owned())
            } else {
                let row = sqlx::query("INSERT INTO litecr.person (public_id,registry_id,organization_id) VALUES ('per-' || gen_random_uuid()::text,$1,$2) RETURNING person_id::text AS person_id,public_id")
                    .bind(&command.registry_id).bind(&command.organization_id)
                    .fetch_one(&mut **tx).await.map_err(|error| {
                        if is_unique_violation(&error) { MigrationError::IdentityCollision } else { MigrationError::CommitFailed }
                    })?;
                (
                    row.try_get("person_id")
                        .map_err(|_| MigrationError::CommitFailed)?,
                    row.try_get("public_id")
                        .map_err(|_| MigrationError::CommitFailed)?,
                )
            };
            let source_id = create_source_record(tx, command, &person_id).await?;
            (source_id, person_id, public_id, true, true, None)
        }
    };

    // The ordinary Registry writer treats an exact source-version/payload
    // match as a replay and does not create another receive version. This
    // importer refuses that cross-system replay because the current migration
    // journal has no flag distinguishing pre-existing versions from versions
    // created by this manifest; silently journaling it would make reversal
    // appear safe when it could delete pre-existing data.
    let existing_exact_version = sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM litecr.source_record_version WHERE source_record_id=$1::uuid AND source_version IS NOT DISTINCT FROM $2 AND payload_hash=$3)")
        .bind(&source_id)
        .bind(&command.source_version)
        .bind(fingerprint.to_vec())
        .fetch_one(&mut **tx)
        .await
        .map_err(|_| MigrationError::CommitFailed)?;
    if existing_exact_version {
        return Err(MigrationError::CheckpointConflict);
    }

    let receive_version: i64 = sqlx::query_scalar("SELECT COALESCE(MAX(receive_version),0)+1 FROM litecr.source_record_version WHERE source_record_id=$1::uuid")
        .bind(&source_id).fetch_one(&mut **tx).await.map_err(|_|MigrationError::CommitFailed)?;

    let prior_source_version_hash = sqlx::query_scalar::<_, Vec<u8>>("SELECT payload_hash FROM litecr.source_record_version WHERE source_record_id=$1::uuid AND source_version IS NOT DISTINCT FROM $2 ORDER BY receive_version DESC LIMIT 1 FOR UPDATE")
        .bind(&source_id)
        .bind(&command.source_version)
        .fetch_optional(&mut **tx)
        .await
        .map_err(|_| MigrationError::CommitFailed)?;
    let mut reason_code = prior_source_version_hash
        .as_deref()
        .filter(|prior_hash| *prior_hash != fingerprint)
        .map(|_| "SOURCE_VERSION_CONFLICT");
    let mut conflict = reason_code.is_some();
    let mut authoritative = command
        .identifiers
        .iter()
        .filter(|identifier| identifier.authoritative)
        .collect::<Vec<_>>();
    authoritative.sort_by(|left, right| {
        (&left.authority, &left.identifier_type, &left.value).cmp(&(
            &right.authority,
            &right.identifier_type,
            &right.value,
        ))
    });
    for identifier in authoritative {
        let lock = format!(
            "{}:{}:{}:{}:{}:{}:{}:{}",
            command.registry_id.len(),
            command.registry_id,
            identifier.authority.len(),
            identifier.authority,
            identifier.identifier_type.len(),
            identifier.identifier_type,
            identifier.value.len(),
            identifier.value
        );
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
            .bind(lock)
            .execute(&mut **tx)
            .await
            .map_err(|_| MigrationError::CommitFailed)?;
        let owner = sqlx::query_scalar::<_, String>("SELECT person_id::text FROM litecr.person_identifier WHERE registry_id=$1 AND authority=$2 AND identifier_type=$3 AND identifier_value=$4 AND person_id<>$5::uuid FOR UPDATE")
            .bind(&command.registry_id).bind(&identifier.authority).bind(&identifier.identifier_type).bind(&identifier.value).bind(&person_id).fetch_optional(&mut **tx).await.map_err(|_|MigrationError::CommitFailed)?;
        if owner.is_some() {
            conflict = true;
            reason_code.get_or_insert("AUTHORITATIVE_IDENTIFIER_CONFLICT");
        }
    }
    let outcome = if conflict {
        "source_conflict"
    } else {
        "accepted"
    };
    let mut introduced_alias_ids = if outcome == "accepted" {
        ensure_aliases(tx, command, &person_id, &public_id, mapped).await?
    } else {
        Vec::new()
    };
    // The journal replay reads these IDs ordered by UUID. Canonicalize the
    // first receipt the same way so commit and retry have identical values.
    introduced_alias_ids.sort_unstable();
    sqlx::query("INSERT INTO litecr.source_record_version (source_record_id,receive_version,source_version,payload_hash,version_state) VALUES ($1::uuid,$2,$3,$4,$5)")
        .bind(&source_id).bind(receive_version).bind(&command.source_version).bind(fingerprint.to_vec()).bind(outcome)
        .execute(&mut **tx).await.map_err(|_|MigrationError::CommitFailed)?;
    for assertion in &command.assertions {
        sqlx::query("INSERT INTO litecr.source_assertion (source_record_id,receive_version,field_path,original_value,language_tag,script) VALUES ($1::uuid,$2,$3,$4,$5,$6)")
            .bind(&source_id).bind(receive_version).bind(&assertion.field_path).bind(&assertion.original_value).bind(&assertion.language_tag).bind(&assertion.script)
            .execute(&mut **tx).await.map_err(|_|MigrationError::CommitFailed)?;
    }
    let mut introduced_identifier_ids = Vec::new();
    for identifier in &command.identifiers {
        let identifier_conflict = identifier.authoritative
            && sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM litecr.person_identifier WHERE registry_id=$1 AND authority=$2 AND identifier_type=$3 AND identifier_value=$4 AND person_id<>$5::uuid)")
                .bind(&command.registry_id).bind(&identifier.authority).bind(&identifier.identifier_type).bind(&identifier.value).bind(&person_id).fetch_one(&mut **tx).await.map_err(|_|MigrationError::CommitFailed)?;
        let assertion_state = if identifier_conflict || (conflict && identifier.authoritative) {
            "conflict"
        } else {
            "accepted"
        };
        sqlx::query("INSERT INTO litecr.source_identifier_assertion (source_record_id,receive_version,registry_id,organization_id,authority,identifier_type,original_value,authoritative,assertion_state) VALUES ($1::uuid,$2,$3,$4,$5,$6,$7,$8,$9)")
            .bind(&source_id).bind(receive_version).bind(&command.registry_id).bind(&command.organization_id).bind(&identifier.authority).bind(&identifier.identifier_type).bind(&identifier.value).bind(identifier.authoritative).bind(assertion_state)
            .execute(&mut **tx).await.map_err(|_|MigrationError::CommitFailed)?;
        if identifier.authoritative && !conflict && !identifier_conflict {
            let inserted = sqlx::query_scalar::<_, String>("INSERT INTO litecr.person_identifier (registry_id,organization_id,authority,identifier_type,identifier_value,person_id,source_record_id,authoritative) VALUES ($1,$2,$3,$4,$5,$6::uuid,$7::uuid,TRUE) ON CONFLICT DO NOTHING RETURNING identifier_id::text")
                .bind(&command.registry_id).bind(&command.organization_id).bind(&identifier.authority).bind(&identifier.identifier_type).bind(&identifier.value).bind(&person_id).bind(&source_id)
                .fetch_optional(&mut **tx).await.map_err(|_|MigrationError::CommitFailed)?;
            if let Some(identifier_id) = inserted {
                introduced_identifier_ids.push(identifier_id);
            }
        }
    }
    introduced_identifier_ids.sort_unstable();
    if outcome == "accepted" {
        index_accepted_source_version(
            tx,
            &source_id,
            &command.registry_id,
            &command.organization_id,
            receive_version,
            &command.assertions,
            &command.identifiers,
        )
        .await
        .map_err(|_| MigrationError::CommitFailed)?;
        sqlx::query("UPDATE litecr.source_record SET current_receive_version=$2,updated_at=now() WHERE source_record_id=$1::uuid")
            .bind(&source_id).bind(receive_version).execute(&mut **tx).await.map_err(|_|MigrationError::CommitFailed)?;
        if !is_new_person {
            sqlx::query("UPDATE litecr.person SET resource_version=resource_version+1 WHERE person_id=$1::uuid")
                .bind(&person_id).execute(&mut **tx).await.map_err(|_|MigrationError::CommitFailed)?;
        }
    }
    let action = if conflict {
        "registry.source_record.conflict"
    } else {
        "registry.source_record.accept"
    };
    sqlx::query("INSERT INTO litecr.mutation_audit (actor_kind,actor_issuer_url,actor_id,registry_id,organization_id,action,resource_type,resource_ref,outcome,reason_code,request_id,correlation_id,resource_version) VALUES ($1,$2,$3,$4,$5,$6,'source_record',$7,$8,$9,$10,$11,$12)")
        .bind(actor_kind).bind(issuer).bind(&actor.id).bind(&command.registry_id).bind(&command.organization_id).bind(action).bind(&source_id).bind(outcome).bind(reason_code).bind(&actor.request_id).bind(&actor.correlation_id).bind(receive_version)
        .execute(&mut **tx).await.map_err(|_|MigrationError::CommitFailed)?;
    let event = if conflict {
        "registry.source_record.conflict"
    } else {
        "registry.source_record.accepted"
    };
    sqlx::query("INSERT INTO litecr.outbox_event (event_type,schema_version,registry_id,organization_id,aggregate_type,aggregate_id,aggregate_version,actor_kind,actor_id,correlation_id,payload) VALUES ($1,1,$2,$3,'source_record',$4::uuid,$5,$6,$7,$8,$9)")
        .bind(event).bind(&command.registry_id).bind(&command.organization_id).bind(&source_id).bind(receive_version).bind(actor_kind).bind(&actor.id).bind(&actor.correlation_id)
        .bind(json!({"source_system_id":command.source_system_id,"source_record_id":source_id,"source_receive_version":receive_version,"person_public_id":public_id,"outcome":outcome,"migration_manifest_sha256":hex(&run.manifest_sha256)}))
        .execute(&mut **tx).await.map_err(|_|MigrationError::CommitFailed)?;
    sqlx::query("INSERT INTO litecr.registry_idempotency (actor_kind,actor_issuer_url,actor_id,operation,registry_id,idempotency_key_hash,command_fingerprint,person_public_id,source_record_id,source_receive_version,outcome) VALUES ($1,$2,$3,'source.upsert',$4,$5,$6,$7,$8::uuid,$9,$10)")
        .bind(actor_kind).bind(issuer).bind(&actor.id).bind(&command.registry_id).bind(request_key).bind(fingerprint.to_vec()).bind(&public_id).bind(&source_id).bind(receive_version).bind(outcome)
        .execute(&mut **tx).await.map_err(|_|MigrationError::CommitFailed)?;

    let current_version = if outcome == "accepted" {
        receive_version
    } else {
        current
            .as_ref()
            .map(|row| {
                row.try_get::<i64, _>("current_receive_version")
                    .unwrap_or(receive_version)
            })
            .unwrap_or(receive_version)
    };
    let current_hash = sqlx::query_scalar::<_, Vec<u8>>("SELECT payload_hash FROM litecr.source_record_version WHERE source_record_id=$1::uuid AND receive_version=$2")
        .bind(&source_id).bind(current_version).fetch_one(&mut **tx).await.map_err(|_|MigrationError::CommitFailed)?;
    let expected_current_sha256 = current_hash
        .try_into()
        .map_err(|_| MigrationError::CommitFailed)?;
    Ok(AppliedSource {
        source_system_id: record.source_system_id.clone(),
        source_record_key: record.source_record_key.clone(),
        source_version: record.source_version.clone(),
        payload_sha256: record.payload_sha256,
        public_id,
        source_record_id: source_id,
        receive_version,
        expected_current_sha256,
        expected_current_receive_version: current_version,
        source_record_created,
        person_created: is_new_person,
        previous_receive_version,
        introduced_alias_ids,
        introduced_identifier_ids,
    })
}

async fn create_source_record(
    tx: &mut sqlx::Transaction<'_, Postgres>,
    command: &SourceRecordCommand,
    person_id: &str,
) -> Result<String, MigrationError> {
    sqlx::query_scalar::<_, String>("INSERT INTO litecr.source_record (registry_id,organization_id,source_system_id,source_record_key,person_id) VALUES ($1,$2,$3,$4,$5::uuid) RETURNING source_record_id::text")
        .bind(&command.registry_id).bind(&command.organization_id).bind(&command.source_system_id).bind(&command.source_record_key).bind(person_id)
        .fetch_one(&mut **tx).await.map_err(|error| {
            if is_unique_violation(&error) { MigrationError::IdentityCollision } else { MigrationError::CommitFailed }
        })
}

async fn ensure_aliases(
    tx: &mut sqlx::Transaction<'_, Postgres>,
    command: &SourceRecordCommand,
    person_id: &str,
    public_id: &str,
    mapped: &MappedMigrationSource,
) -> Result<Vec<String>, MigrationError> {
    let mut aliases = Vec::with_capacity(mapped.aliases.len() + 2);
    if let Some(cruid) = &mapped.preserved_cruid {
        aliases.push(cruid.as_str());
    }
    aliases.extend(mapped.aliases.iter().map(String::as_str));
    // The public ID is the person column's stable key; also make it resolvable
    // through the same alias endpoint used by the existing Registry adapter.
    aliases.push(public_id);
    aliases.sort_unstable();
    aliases.dedup();
    let mut introduced = Vec::new();
    for alias in aliases {
        let existing = sqlx::query("SELECT person_id::text AS person_id FROM litecr.person_alias WHERE registry_id=$1 AND organization_id=$2 AND alias_value=$3 FOR UPDATE")
            .bind(&command.registry_id).bind(&command.organization_id).bind(alias).fetch_optional(&mut **tx).await.map_err(|_|MigrationError::CommitFailed)?;
        if let Some(row) = existing {
            let owner: String = row
                .try_get("person_id")
                .map_err(|_| MigrationError::CommitFailed)?;
            if owner != person_id {
                return Err(MigrationError::IdentityCollision);
            }
            continue;
        }
        let alias_id = sqlx::query_scalar::<_, String>("INSERT INTO litecr.person_alias (registry_id,organization_id,alias_value,person_id) VALUES ($1,$2,$3,$4::uuid) RETURNING alias_id::text")
            .bind(&command.registry_id).bind(&command.organization_id).bind(alias).bind(person_id).fetch_one(&mut **tx).await.map_err(|error| {
                if is_unique_violation(&error) { MigrationError::IdentityCollision } else { MigrationError::CommitFailed }
            })?;
        introduced.push(alias_id);
    }
    Ok(introduced)
}

fn is_unique_violation(error: &sqlx::Error) -> bool {
    matches!(error, sqlx::Error::Database(database) if database.code().as_deref() == Some("23505"))
}

fn hex(bytes: &[u8]) -> String {
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(&mut encoded, "{byte:02x}").expect("writing to String cannot fail");
    }
    encoded
}
