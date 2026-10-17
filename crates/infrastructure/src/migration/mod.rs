//! Transaction-enlistable persistence for the migration checkpoint and journal.
//! Registry import adapters call these methods using the same SQLx transaction
//! in which the ordinary authorized Registry source use case commits.

mod executor;
mod package_v2;
pub use executor::PostgresMigrationExecutor;

use litecr_application::migration::{
    AppliedSource, BatchCommit, MigrationBatch, MigrationCheckpoint, MigrationError, MigrationRun,
    MigrationRunState,
};
use litecr_domain::registry::{IdentifierAssertion, SourceAssertion, SourceRecordCommand};
use serde_json::Value;
use sqlx::{PgPool, Postgres, Row, Transaction};
use thiserror::Error;

#[derive(Clone)]
pub struct PostgresMigrationJournal {
    pool: PgPool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MappedMigrationSource {
    pub command: SourceRecordCommand,
    pub preserved_public_id: Option<String>,
    pub preserved_cruid: Option<String>,
    pub aliases: Vec<String>,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum MigrationRowRefusal {
    #[error("migration row JSON is invalid")]
    InvalidJson,
    #[error("migration row has a field unsupported by the Registry source model")]
    UnsupportedField,
    #[error("historical links and non-match facts are not activated by this importer")]
    HistoricalRelationshipsUnsupported,
    #[error("migration row identifiers/aliases are inconsistent")]
    InvalidAliases,
    #[error("migration row does not satisfy Registry source constraints")]
    RegistryValidation,
}

/// Map only fields that the current Registry schema can preserve. The exact
/// original row remains in the journal. Partial-date precision metadata is
/// retained there; `original_value` itself is copied unchanged into the
/// source assertion. Historical links/non-matches are refused, never inferred.
pub fn map_supported_record(
    run: &MigrationRun,
    record: &litecr_application::migration::MigrationRecord,
) -> Result<MappedMigrationSource, MigrationRowRefusal> {
    let raw = record
        .raw_ndjson
        .strip_suffix(b"\n")
        .unwrap_or(&record.raw_ndjson);
    let row: Value = serde_json::from_slice(raw).map_err(|_| MigrationRowRefusal::InvalidJson)?;
    let object = row.as_object().ok_or(MigrationRowRefusal::InvalidJson)?;
    const ROW_FIELDS: &[&str] = &[
        "source_system_id",
        "source_record_key",
        "source_version",
        "public_id",
        "cruid",
        "aliases",
        "assertions",
        "identifiers",
        "relationships",
    ];
    if object.keys().any(|key| !ROW_FIELDS.contains(&key.as_str())) {
        return Err(MigrationRowRefusal::UnsupportedField);
    }
    if object.get("source_system_id").and_then(Value::as_str) != Some(&run.source_system_id)
        || object.get("source_system_id").and_then(Value::as_str) != Some(&record.source_system_id)
        || object.get("source_record_key").and_then(Value::as_str)
            != Some(&record.source_record_key)
        || object.get("source_version").and_then(Value::as_str) != Some(&record.source_version)
    {
        return Err(MigrationRowRefusal::InvalidJson);
    }
    let assertions = object
        .get("assertions")
        .and_then(Value::as_array)
        .ok_or(MigrationRowRefusal::RegistryValidation)?;
    let mut mapped_assertions = Vec::with_capacity(assertions.len());
    for assertion in assertions {
        let item = assertion
            .as_object()
            .ok_or(MigrationRowRefusal::RegistryValidation)?;
        const ASSERTION_FIELDS: &[&str] = &[
            "field_path",
            "original_value",
            "language_tag",
            "script",
            "partial_date_precision",
        ];
        if item
            .keys()
            .any(|key| !ASSERTION_FIELDS.contains(&key.as_str()))
        {
            return Err(MigrationRowRefusal::UnsupportedField);
        }
        let partial_date_precision = optional_string(item.get("partial_date_precision"))?;
        if partial_date_precision
            .as_deref()
            .is_some_and(|precision| !matches!(precision, "unknown" | "year" | "month" | "day"))
        {
            return Err(MigrationRowRefusal::RegistryValidation);
        }
        mapped_assertions.push(SourceAssertion {
            field_path: item
                .get("field_path")
                .and_then(Value::as_str)
                .ok_or(MigrationRowRefusal::RegistryValidation)?
                .to_owned(),
            original_value: item
                .get("original_value")
                .and_then(Value::as_str)
                .ok_or(MigrationRowRefusal::RegistryValidation)?
                .to_owned(),
            language_tag: optional_string(item.get("language_tag"))?,
            script: optional_string(item.get("script"))?,
        });
    }
    let identifiers = object
        .get("identifiers")
        .and_then(Value::as_array)
        .ok_or(MigrationRowRefusal::RegistryValidation)?;
    let mut mapped_identifiers = Vec::with_capacity(identifiers.len());
    for identifier in identifiers {
        let item = identifier
            .as_object()
            .ok_or(MigrationRowRefusal::RegistryValidation)?;
        const IDENTIFIER_FIELDS: &[&str] = &["authority", "type", "value", "authoritative"];
        if item
            .keys()
            .any(|key| !IDENTIFIER_FIELDS.contains(&key.as_str()))
        {
            return Err(MigrationRowRefusal::UnsupportedField);
        }
        mapped_identifiers.push(IdentifierAssertion {
            authority: item
                .get("authority")
                .and_then(Value::as_str)
                .ok_or(MigrationRowRefusal::RegistryValidation)?
                .to_owned(),
            identifier_type: item
                .get("type")
                .and_then(Value::as_str)
                .ok_or(MigrationRowRefusal::RegistryValidation)?
                .to_owned(),
            value: item
                .get("value")
                .and_then(Value::as_str)
                .ok_or(MigrationRowRefusal::RegistryValidation)?
                .to_owned(),
            authoritative: item
                .get("authoritative")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        });
    }
    match object.get("relationships") {
        None => {}
        Some(Value::Array(relationships)) if relationships.is_empty() => {}
        Some(Value::Array(_)) => {
            return Err(MigrationRowRefusal::HistoricalRelationshipsUnsupported);
        }
        Some(_) => return Err(MigrationRowRefusal::InvalidJson),
    }
    let preserved_public_id = optional_string(object.get("public_id"))?;
    let preserved_cruid = optional_string(object.get("cruid"))?;
    let aliases = object
        .get("aliases")
        .map(|value| {
            value
                .as_array()
                .ok_or(MigrationRowRefusal::InvalidAliases)?
                .iter()
                .map(|alias| {
                    alias
                        .as_str()
                        .map(str::to_owned)
                        .ok_or(MigrationRowRefusal::InvalidAliases)
                })
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()?
        .unwrap_or_default();
    let mut seen = std::collections::BTreeSet::new();
    for alias in aliases
        .iter()
        .chain(preserved_cruid.iter())
        .chain(preserved_public_id.iter())
    {
        if alias.trim().is_empty() || alias.len() > 512 || !seen.insert(alias) {
            return Err(MigrationRowRefusal::InvalidAliases);
        }
    }
    let command = SourceRecordCommand {
        registry_id: run.registry_id.clone(),
        organization_id: run.organization_id.clone(),
        source_system_id: record.source_system_id.clone(),
        source_record_key: record.source_record_key.clone(),
        source_version: Some(record.source_version.clone()),
        assertions: mapped_assertions,
        identifiers: mapped_identifiers,
    };
    command
        .validate()
        .map_err(|_| MigrationRowRefusal::RegistryValidation)?;
    Ok(MappedMigrationSource {
        command,
        preserved_public_id,
        preserved_cruid,
        aliases,
    })
}

fn optional_string(value: Option<&Value>) -> Result<Option<String>, MigrationRowRefusal> {
    match value {
        None => Ok(None),
        Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(MigrationRowRefusal::RegistryValidation),
    }
}

impl PostgresMigrationJournal {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Establishes or verifies an exact-manifest-bound checkpoint. This does
    /// not import a source record and is safe to retry.
    pub async fn establish(
        &self,
        run: &MigrationRun,
        actor_id: &str,
    ) -> Result<MigrationCheckpoint, MigrationError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|_| MigrationError::CommitFailed)?;
        let result = Self::establish_in_transaction(&mut tx, run, actor_id).await?;
        tx.commit()
            .await
            .map_err(|_| MigrationError::CommitFailed)?;
        Ok(result)
    }

    pub async fn establish_in_transaction(
        tx: &mut Transaction<'_, Postgres>,
        run: &MigrationRun,
        actor_id: &str,
    ) -> Result<MigrationCheckpoint, MigrationError> {
        if actor_id.trim().is_empty() || run.expected_record_count > i64::MAX as u64 {
            return Err(MigrationError::InvalidBatch);
        }
        sqlx::query("INSERT INTO litecr.migration_checkpoint (manifest_sha256,stream_sha256,snapshot_id,source_system_id,registry_id,organization_id,expected_record_count,state,created_by) VALUES ($1,$2,$3,$4,$5,$6,$7,'validated',$8) ON CONFLICT (manifest_sha256) DO NOTHING")
            .bind(run.manifest_sha256.to_vec()).bind(run.stream_sha256.to_vec()).bind(&run.snapshot_id).bind(&run.source_system_id).bind(&run.registry_id).bind(&run.organization_id).bind(run.expected_record_count as i64).bind(actor_id)
            .execute(&mut **tx).await.map_err(|_|MigrationError::CommitFailed)?;
        let row = sqlx::query("SELECT stream_sha256,snapshot_id,source_system_id,registry_id,organization_id,expected_record_count,committed_batch_sequence,committed_record_count,state FROM litecr.migration_checkpoint WHERE manifest_sha256=$1 FOR UPDATE")
            .bind(run.manifest_sha256.to_vec()).fetch_one(&mut **tx).await.map_err(|_|MigrationError::CheckpointConflict)?;
        let consistent: bool = row.try_get::<Vec<u8>, _>("stream_sha256").ok().as_deref()
            == Some(run.stream_sha256.as_slice())
            && row.try_get::<String, _>("snapshot_id").ok().as_deref()
                == Some(run.snapshot_id.as_str())
            && row.try_get::<String, _>("source_system_id").ok().as_deref()
                == Some(run.source_system_id.as_str())
            && row.try_get::<String, _>("registry_id").ok().as_deref()
                == Some(run.registry_id.as_str())
            && row.try_get::<String, _>("organization_id").ok().as_deref()
                == Some(run.organization_id.as_str())
            && row.try_get::<i64, _>("expected_record_count").ok()
                == Some(run.expected_record_count as i64);
        if !consistent {
            return Err(MigrationError::CheckpointConflict);
        }
        decode_checkpoint(&row, run.manifest_sha256)
    }

    /// Record the batch and every importer receipt. Call after Registry use
    /// cases have written their changes, but before committing that *same*
    /// transaction. A caller must not split these operations across commits.
    pub async fn record_batch_in_transaction(
        tx: &mut Transaction<'_, Postgres>,
        batch: &MigrationBatch,
        actor_id: &str,
        applied: &[AppliedSource],
    ) -> Result<BatchCommit, MigrationError> {
        litecr_application::migration::validate_batch(batch)?;
        if actor_id.trim().is_empty() {
            return Err(MigrationError::InvalidBatch);
        }
        let checkpoint = Self::establish_in_transaction(tx, &batch.run, actor_id).await?;
        if batch.sequence <= checkpoint.committed_batch_sequence {
            let stored = sqlx::query("SELECT batch_sha256 FROM litecr.migration_batch WHERE manifest_sha256=$1 AND batch_sequence=$2")
                .bind(batch.run.manifest_sha256.to_vec()).bind(batch.sequence as i64).fetch_optional(&mut **tx).await.map_err(|_|MigrationError::CommitFailed)?;
            if stored
                .as_ref()
                .and_then(|r| r.try_get::<Vec<u8>, _>("batch_sha256").ok())
                .as_deref()
                != Some(batch.batch_sha256.as_slice())
            {
                return Err(MigrationError::CheckpointConflict);
            }
            let rows = sqlx::query("SELECT source_system_id,source_record_key,source_version,source_payload_sha256,imported_public_id AS public_id,imported_source_record_id::text AS source_record_id,imported_receive_version,expected_current_payload_sha256,expected_current_receive_version,source_record_created,person_created,previous_receive_version FROM litecr.migration_journal WHERE manifest_sha256=$1 AND batch_sequence=$2 ORDER BY source_system_id,source_record_key,source_version")
                .bind(batch.run.manifest_sha256.to_vec()).bind(batch.sequence as i64).fetch_all(&mut **tx).await.map_err(|_|MigrationError::CommitFailed)?;
            let mut existing = rows
                .into_iter()
                .map(decode_applied)
                .collect::<Result<Vec<_>, _>>()?;
            if existing.len() != batch.records.len() {
                return Err(MigrationError::CheckpointConflict);
            }
            for receipt in &mut existing {
                let effects = sqlx::query("SELECT effect_kind,effect_ref::text AS effect_ref FROM litecr.migration_effect WHERE manifest_sha256=$1 AND batch_sequence=$2 AND source_record_id=$3::uuid ORDER BY effect_kind,effect_ref")
                    .bind(batch.run.manifest_sha256.to_vec())
                    .bind(batch.sequence as i64)
                    .bind(&receipt.source_record_id)
                    .fetch_all(&mut **tx)
                    .await
                    .map_err(|_| MigrationError::CommitFailed)?;
                for effect in effects {
                    let kind: String = effect
                        .try_get("effect_kind")
                        .map_err(|_| MigrationError::CommitFailed)?;
                    let id: String = effect
                        .try_get("effect_ref")
                        .map_err(|_| MigrationError::CommitFailed)?;
                    match kind.as_str() {
                        "person_alias" => receipt.introduced_alias_ids.push(id),
                        "person_identifier" => receipt.introduced_identifier_ids.push(id),
                        _ => return Err(MigrationError::CommitFailed),
                    }
                }
            }
            return Ok(BatchCommit {
                checkpoint,
                applied: existing,
            });
        }
        if applied.len() != batch.records.len() {
            return Err(MigrationError::InvalidBatch);
        }
        if batch.sequence != checkpoint.committed_batch_sequence + 1
            || applied.iter().any(|receipt| {
                !batch.records.iter().any(|record| {
                    record.source_system_id == receipt.source_system_id
                        && record.source_record_key == receipt.source_record_key
                        && record.source_version == receipt.source_version
                        && record.payload_sha256 == receipt.payload_sha256
                })
            })
        {
            return Err(MigrationError::CheckpointConflict);
        }
        let first_line =
            i64::try_from(batch.first_input_line).map_err(|_| MigrationError::InvalidBatch)?;
        let last_line =
            i64::try_from(batch.last_input_line).map_err(|_| MigrationError::InvalidBatch)?;
        sqlx::query("INSERT INTO litecr.migration_batch (manifest_sha256,batch_sequence,stream_sha256,batch_sha256,record_count,first_input_line,last_input_line,committed_by) VALUES ($1,$2,$3,$4,$5,$6,$7,$8)")
            .bind(batch.run.manifest_sha256.to_vec()).bind(batch.sequence as i64).bind(batch.run.stream_sha256.to_vec()).bind(batch.batch_sha256.to_vec()).bind(batch.records.len() as i32).bind(first_line).bind(last_line).bind(actor_id)
            .execute(&mut **tx).await.map_err(|_|MigrationError::CommitFailed)?;
        for receipt in applied {
            let record = batch
                .records
                .iter()
                .find(|record| {
                    record.source_system_id == receipt.source_system_id
                        && record.source_record_key == receipt.source_record_key
                        && record.source_version == receipt.source_version
                })
                .ok_or(MigrationError::CheckpointConflict)?;
            if receipt.receive_version <= 0 || receipt.expected_current_receive_version <= 0 {
                return Err(MigrationError::InvalidBatch);
            }
            let raw = record
                .raw_ndjson
                .strip_suffix(b"\n")
                .unwrap_or(&record.raw_ndjson);
            let row: serde_json::Value =
                serde_json::from_slice(raw).map_err(|_| MigrationError::InvalidBatch)?;
            let public_id = row.get("public_id").and_then(serde_json::Value::as_str);
            let cruid = row.get("cruid").and_then(serde_json::Value::as_str);
            sqlx::query("INSERT INTO litecr.migration_journal (manifest_sha256,batch_sequence,source_system_id,source_record_key,source_version,source_payload_sha256,original_payload_ndjson,preserved_public_id,preserved_cruid,imported_public_id,imported_source_record_id,imported_receive_version,expected_current_payload_sha256,expected_current_receive_version,source_record_created,person_created,previous_receive_version) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11::uuid,$12,$13,$14,$15,$16,$17)")
                .bind(batch.run.manifest_sha256.to_vec()).bind(batch.sequence as i64).bind(&receipt.source_system_id).bind(&receipt.source_record_key).bind(&receipt.source_version).bind(receipt.payload_sha256.to_vec()).bind(&record.raw_ndjson).bind(public_id).bind(cruid).bind(&receipt.public_id).bind(&receipt.source_record_id).bind(receipt.receive_version).bind(receipt.expected_current_sha256.to_vec()).bind(receipt.expected_current_receive_version).bind(receipt.source_record_created).bind(receipt.person_created).bind(receipt.previous_receive_version)
                .execute(&mut **tx).await.map_err(|_|MigrationError::CommitFailed)?;
            for (kind, ids) in [
                ("person_alias", receipt.introduced_alias_ids.as_slice()),
                (
                    "person_identifier",
                    receipt.introduced_identifier_ids.as_slice(),
                ),
            ] {
                for effect_ref in ids {
                    sqlx::query("INSERT INTO litecr.migration_effect (manifest_sha256,batch_sequence,source_record_id,effect_kind,effect_ref) VALUES ($1,$2,$3::uuid,$4,$5::uuid)")
                        .bind(batch.run.manifest_sha256.to_vec())
                        .bind(batch.sequence as i64)
                        .bind(&receipt.source_record_id)
                        .bind(kind)
                        .bind(effect_ref)
                        .execute(&mut **tx)
                        .await
                        .map_err(|_| MigrationError::CommitFailed)?;
                }
            }
        }
        let committed = checkpoint.committed_record_count + applied.len() as u64;
        if committed > batch.run.expected_record_count {
            return Err(MigrationError::CheckpointConflict);
        }
        let state = if committed == batch.run.expected_record_count {
            "completed"
        } else {
            "importing"
        };
        sqlx::query("UPDATE litecr.migration_checkpoint SET committed_batch_sequence=$2,committed_record_count=$3,state=$4,updated_at=now() WHERE manifest_sha256=$1")
            .bind(batch.run.manifest_sha256.to_vec()).bind(batch.sequence as i64).bind(committed as i64).bind(state)
            .execute(&mut **tx).await.map_err(|_|MigrationError::CommitFailed)?;
        Ok(BatchCommit {
            checkpoint: MigrationCheckpoint {
                manifest_sha256: batch.run.manifest_sha256,
                committed_batch_sequence: batch.sequence,
                committed_record_count: committed,
                state: if state == "completed" {
                    MigrationRunState::Completed
                } else {
                    MigrationRunState::Importing
                },
            },
            applied: applied.to_vec(),
        })
    }

    pub async fn checkpoint(
        &self,
        run: &MigrationRun,
    ) -> Result<Option<MigrationCheckpoint>, MigrationError> {
        let row = sqlx::query("SELECT committed_batch_sequence,committed_record_count,state FROM litecr.migration_checkpoint WHERE manifest_sha256=$1 AND stream_sha256=$2 AND snapshot_id=$3 AND source_system_id=$4 AND registry_id=$5 AND organization_id=$6 AND expected_record_count=$7")
            .bind(run.manifest_sha256.to_vec()).bind(run.stream_sha256.to_vec()).bind(&run.snapshot_id).bind(&run.source_system_id).bind(&run.registry_id).bind(&run.organization_id).bind(run.expected_record_count as i64)
            .fetch_optional(&self.pool).await.map_err(|_|MigrationError::CommitFailed)?;
        row.map(|row| decode_checkpoint(&row, run.manifest_sha256))
            .transpose()
    }
}

fn decode_checkpoint(
    row: &sqlx::postgres::PgRow,
    manifest_sha256: [u8; 32],
) -> Result<MigrationCheckpoint, MigrationError> {
    let state: String = row
        .try_get("state")
        .map_err(|_| MigrationError::CommitFailed)?;
    Ok(MigrationCheckpoint {
        manifest_sha256,
        committed_batch_sequence: row
            .try_get::<i64, _>("committed_batch_sequence")
            .map_err(|_| MigrationError::CommitFailed)? as u64,
        committed_record_count: row
            .try_get::<i64, _>("committed_record_count")
            .map_err(|_| MigrationError::CommitFailed)? as u64,
        state: match state.as_str() {
            "validated" => MigrationRunState::Validated,
            "importing" => MigrationRunState::Importing,
            "completed" => MigrationRunState::Completed,
            "reverse_blocked" => MigrationRunState::ReverseBlocked,
            "reversed" => MigrationRunState::Reversed,
            _ => return Err(MigrationError::CommitFailed),
        },
    })
}

fn decode_applied(row: sqlx::postgres::PgRow) -> Result<AppliedSource, MigrationError> {
    let source_payload_sha256: Vec<u8> = row
        .try_get("source_payload_sha256")
        .map_err(|_| MigrationError::CommitFailed)?;
    let expected_current_sha256: Vec<u8> = row
        .try_get("expected_current_payload_sha256")
        .map_err(|_| MigrationError::CommitFailed)?;
    Ok(AppliedSource {
        source_system_id: row
            .try_get("source_system_id")
            .map_err(|_| MigrationError::CommitFailed)?,
        source_record_key: row
            .try_get("source_record_key")
            .map_err(|_| MigrationError::CommitFailed)?,
        source_version: row
            .try_get("source_version")
            .map_err(|_| MigrationError::CommitFailed)?,
        payload_sha256: source_payload_sha256
            .try_into()
            .map_err(|_| MigrationError::CommitFailed)?,
        public_id: row
            .try_get("public_id")
            .map_err(|_| MigrationError::CommitFailed)?,
        source_record_id: row
            .try_get("source_record_id")
            .map_err(|_| MigrationError::CommitFailed)?,
        receive_version: row
            .try_get("imported_receive_version")
            .map_err(|_| MigrationError::CommitFailed)?,
        expected_current_sha256: expected_current_sha256
            .try_into()
            .map_err(|_| MigrationError::CommitFailed)?,
        expected_current_receive_version: row
            .try_get("expected_current_receive_version")
            .map_err(|_| MigrationError::CommitFailed)?,
        source_record_created: row
            .try_get("source_record_created")
            .map_err(|_| MigrationError::CommitFailed)?,
        person_created: row
            .try_get("person_created")
            .map_err(|_| MigrationError::CommitFailed)?,
        previous_receive_version: row
            .try_get("previous_receive_version")
            .map_err(|_| MigrationError::CommitFailed)?,
        introduced_alias_ids: Vec::new(),
        introduced_identifier_ids: Vec::new(),
    })
}
