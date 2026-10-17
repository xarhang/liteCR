//! PostgreSQL scoped source-record retrieval and evidence lineage persistence.
use async_trait::async_trait;
use litecr_application::matching::{MatchIdempotency, MatchingStore, MatchingStoreError};
use litecr_application::worker::{ConsumerCompletion, ConsumerError};
use litecr_domain::registry::{IdentifierAssertion, SourceAssertion};
use litecr_domain::worker::{ConsumerLease, ConsumerReceiptCode};
use litecr_matching::{
    MatchAssertion, MatchIdentifier, MatchRecord, MatchResult, NORMALIZATION_VERSION, PartialDate,
    authoritative_identifier_key, is_matchable_original_name_field_path, match_records,
    normalized_assertion_key,
};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Postgres, Row, Transaction};
use std::{
    collections::BTreeMap,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::Semaphore;

#[derive(Clone)]
pub struct PostgresMatchingStore {
    pool: PgPool,
    evaluation_admission: Arc<Semaphore>,
    evaluation_active: Arc<Semaphore>,
}

struct MatchEvaluationRequest<'a> {
    registry_id: &'a str,
    organization_id: &'a str,
    query_source_record_id: &'a str,
    candidate_limit: usize,
    actor_id: &'a str,
    request_id: &'a str,
    idempotency: Option<MatchIdempotency>,
    worker: Option<&'a ConsumerLease>,
}

async fn lookup_idempotent_result<'e, E>(
    executor: E,
    idempotency: MatchIdempotency,
    registry_id: &str,
    organization_id: &str,
    query_source_record_id: &str,
) -> Result<Option<MatchResult>, MatchingStoreError>
where
    E: sqlx::Executor<'e, Database = Postgres>,
{
    let Some(row) = sqlx::query(
        "SELECT i.request_digest,e.run_id::text AS run_id,e.result_snapshot \
         FROM matching.request_idempotency i \
         JOIN matching.evidence_run e ON e.run_id=i.evidence_run_id \
         WHERE i.actor_digest=$1 AND i.registry_id=$2 AND i.organization_id=$3 \
           AND i.query_source_record_id=$4::uuid AND i.key_digest=$5 \
           AND e.registry_id=i.registry_id AND e.organization_id=i.organization_id \
           AND e.query_source_record_id=i.query_source_record_id",
    )
    .bind(idempotency.actor_digest.to_vec())
    .bind(registry_id)
    .bind(organization_id)
    .bind(query_source_record_id)
    .bind(idempotency.key_digest.to_vec())
    .fetch_optional(executor)
    .await
    .map_err(|_| MatchingStoreError::Unavailable)?
    else {
        return Ok(None);
    };
    let request_digest: Vec<u8> = row
        .try_get("request_digest")
        .map_err(|_| MatchingStoreError::Unavailable)?;
    if request_digest.as_slice() != idempotency.request_digest {
        return Err(MatchingStoreError::IdempotencyConflict);
    }
    let run_id: String = row
        .try_get("run_id")
        .map_err(|_| MatchingStoreError::Unavailable)?;
    let snapshot: Option<Value> = row
        .try_get("result_snapshot")
        .map_err(|_| MatchingStoreError::Unavailable)?;
    let mut result: MatchResult =
        serde_json::from_value(snapshot.ok_or(MatchingStoreError::Unavailable)?)
            .map_err(|_| MatchingStoreError::Unavailable)?;
    if result.evidence_run_id.as_deref() != Some(run_id.as_str())
        || result.query_record_id != query_source_record_id
        || !result.snapshot_complete
    {
        return Err(MatchingStoreError::Unavailable);
    }
    result.evidence_run_id = Some(run_id);
    Ok(Some(result))
}

fn idempotency_lock_key(
    idempotency: MatchIdempotency,
    registry_id: &str,
    organization_id: &str,
    query_source_record_id: &str,
) -> i64 {
    let mut digest = Sha256::new();
    digest.update(b"litecr.match.idempotency-lock.v1");
    digest.update(idempotency.actor_digest);
    digest.update(idempotency.key_digest);
    for value in [registry_id, organization_id, query_source_record_id] {
        digest.update((value.len() as u64).to_be_bytes());
        digest.update(value.as_bytes());
    }
    let bytes: [u8; 32] = digest.finalize().into();
    i64::from_be_bytes(
        bytes[..8]
            .try_into()
            .expect("SHA-256 prefix has eight bytes"),
    )
}

impl PostgresMatchingStore {
    pub fn new(pool: PgPool) -> Self {
        let active = pool
            .options()
            .get_max_connections()
            .saturating_sub(2)
            .clamp(1, 2) as usize;
        Self::with_evaluation_parallelism(pool, active).expect("default matching capacity is valid")
    }

    /// Configure active evaluations per store; clones share the same admission gates.
    /// A nominal two-connection margin does not reserve connections from other users
    /// of the shared pool. Independent stores/processes have independent budgets.
    pub fn with_evaluation_parallelism(pool: PgPool, active: usize) -> Result<Self, &'static str> {
        let maximum = pool
            .options()
            .get_max_connections()
            .saturating_sub(2)
            .clamp(1, 128) as usize;
        if active == 0 || active > maximum {
            return Err(
                "matching parallelism must be positive and within the pool budget (maximum pool size minus two, minimum one, capped at 128)",
            );
        }
        Ok(Self {
            pool,
            evaluation_admission: Arc::new(Semaphore::new(128)),
            evaluation_active: Arc::new(Semaphore::new(active)),
        })
    }

    /// Count current accepted source versions that still need the active matcher index.
    /// This is intended for an operator diagnostic; matching remains correct through
    /// the exhaustive compatibility fallback while the count is non-zero.
    pub async fn pending_reindex_count(&self) -> Result<i64, MatchingStoreError> {
        sqlx::query_scalar(
            "SELECT COUNT(*) FROM litecr.source_record sr \
             JOIN litecr.source_record_version sv \
               ON sv.source_record_id=sr.source_record_id AND sv.receive_version=sr.current_receive_version \
             WHERE sr.withdrawn_at IS NULL AND sv.withdrawn_at IS NULL AND sv.version_state='accepted' \
               AND NOT EXISTS (SELECT 1 FROM matching.indexed_source_version ix \
                 WHERE ix.source_record_id=sr.source_record_id AND ix.receive_version=sr.current_receive_version \
                   AND ix.normalizer_version=$1)",
        )
        .bind(NORMALIZATION_VERSION)
        .fetch_one(&self.pool)
        .await
        .map_err(|_| MatchingStoreError::Unavailable)
    }

    /// Index a bounded batch of current accepted versions left by older builds.
    /// Rows are locked and the current version is rechecked in the same transaction;
    /// repeating the command is safe and skips versions already marked complete.
    pub async fn reindex_next_batch(
        &self,
        registry_id: Option<&str>,
        organization_id: Option<&str>,
        batch_size: usize,
    ) -> Result<usize, MatchingStoreError> {
        if !(1..=1000).contains(&batch_size) {
            return Err(MatchingStoreError::Unavailable);
        }
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|_| MatchingStoreError::Unavailable)?;
        let targets = sqlx::query(
            "SELECT sr.source_record_id::text AS source_record_id,sr.registry_id,sr.organization_id,sv.receive_version \
             FROM litecr.source_record sr JOIN litecr.source_record_version sv \
               ON sv.source_record_id=sr.source_record_id AND sv.receive_version=sr.current_receive_version \
             WHERE sr.withdrawn_at IS NULL AND sv.withdrawn_at IS NULL AND sv.version_state='accepted' \
               AND ($1::text IS NULL OR sr.registry_id=$1) AND ($2::text IS NULL OR sr.organization_id=$2) \
               AND NOT EXISTS (SELECT 1 FROM matching.indexed_source_version ix \
                 WHERE ix.source_record_id=sr.source_record_id AND ix.receive_version=sr.current_receive_version \
                   AND ix.normalizer_version=$3) \
             ORDER BY sr.registry_id,sr.organization_id,sr.source_record_id \
             LIMIT $4 FOR UPDATE OF sr SKIP LOCKED",
        )
        .bind(registry_id)
        .bind(organization_id)
        .bind(NORMALIZATION_VERSION)
        .bind(batch_size as i64)
        .fetch_all(&mut *tx)
        .await
        .map_err(|_| MatchingStoreError::Unavailable)?;

        let mut indexed = 0;
        for target in targets {
            let source_record_id: String = target
                .try_get("source_record_id")
                .map_err(|_| MatchingStoreError::Unavailable)?;
            let registry_id: String = target
                .try_get("registry_id")
                .map_err(|_| MatchingStoreError::Unavailable)?;
            let organization_id: String = target
                .try_get("organization_id")
                .map_err(|_| MatchingStoreError::Unavailable)?;
            let receive_version: i64 = target
                .try_get("receive_version")
                .map_err(|_| MatchingStoreError::Unavailable)?;
            let assertions = sqlx::query(
                "SELECT field_path,original_value,language_tag,script FROM litecr.source_assertion \
                 WHERE source_record_id=$1::uuid AND receive_version=$2 AND withdrawn_at IS NULL ORDER BY field_path",
            )
            .bind(&source_record_id)
            .bind(receive_version)
            .fetch_all(&mut *tx)
            .await
            .map_err(|_| MatchingStoreError::Unavailable)?
            .into_iter()
            .map(|row| Ok(SourceAssertion {
                field_path: row.try_get("field_path").map_err(|_| MatchingStoreError::Unavailable)?,
                original_value: row.try_get("original_value").map_err(|_| MatchingStoreError::Unavailable)?,
                language_tag: row.try_get("language_tag").map_err(|_| MatchingStoreError::Unavailable)?,
                script: row.try_get("script").map_err(|_| MatchingStoreError::Unavailable)?,
            }))
            .collect::<Result<Vec<_>, MatchingStoreError>>()?;
            let identifiers = sqlx::query(
                "SELECT authority,identifier_type,original_value AS value,authoritative FROM litecr.source_identifier_assertion \
                 WHERE source_record_id=$1::uuid AND receive_version=$2 AND withdrawn_at IS NULL ORDER BY authority,identifier_type",
            )
            .bind(&source_record_id)
            .bind(receive_version)
            .fetch_all(&mut *tx)
            .await
            .map_err(|_| MatchingStoreError::Unavailable)?
            .into_iter()
            .map(|row| Ok(IdentifierAssertion {
                authority: row.try_get("authority").map_err(|_| MatchingStoreError::Unavailable)?,
                identifier_type: row.try_get("identifier_type").map_err(|_| MatchingStoreError::Unavailable)?,
                value: row.try_get("value").map_err(|_| MatchingStoreError::Unavailable)?,
                authoritative: row.try_get("authoritative").map_err(|_| MatchingStoreError::Unavailable)?,
            }))
            .collect::<Result<Vec<_>, MatchingStoreError>>()?;
            index_accepted_source_version(
                &mut tx,
                &source_record_id,
                &registry_id,
                &organization_id,
                receive_version,
                &assertions,
                &identifiers,
            )
            .await
            .map_err(|_| MatchingStoreError::Unavailable)?;
            indexed += 1;
        }
        tx.commit()
            .await
            .map_err(|_| MatchingStoreError::Unavailable)?;
        Ok(indexed)
    }

    pub async fn evaluate_for_consumer(
        &self,
        lease: &ConsumerLease,
        candidate_limit: usize,
    ) -> Result<ConsumerCompletion, ConsumerError> {
        if !(1..=200).contains(&candidate_limit) {
            return Err(ConsumerError::Unavailable);
        }
        if let Some(receipt) = sqlx::query(
            "SELECT outcome,evidence_run_id::text AS evidence_run_id,receipt_code \
             FROM litecr.consumer_receipt WHERE consumer_name='advisory-matcher-v1' AND event_id=$1::uuid",
        )
        .bind(&lease.event_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| ConsumerError::Unavailable)?
        {
            let outcome: String = receipt
                .try_get("outcome")
                .map_err(|_| ConsumerError::Unavailable)?;
            if outcome == "completed" {
                return Ok(ConsumerCompletion::EvidencePersisted {
                    evidence_run_id: receipt
                        .try_get("evidence_run_id")
                        .map_err(|_| ConsumerError::Unavailable)?,
                });
            }
            if outcome == "obsolete" {
                let code: String = receipt
                    .try_get("receipt_code")
                    .map_err(|_| ConsumerError::Unavailable)?;
                return Ok(ConsumerCompletion::Obsolete {
                    code: receipt_code_from_str(&code),
                });
            }
            return Err(ConsumerError::Unavailable);
        }
        match self
            .evaluate_internal(MatchEvaluationRequest {
                registry_id: &lease.registry_id,
                organization_id: &lease.organization_id,
                query_source_record_id: &lease.source_record_id,
                candidate_limit,
                actor_id: "litecr-advisory-matcher",
                request_id: &format!("consumer:{}", lease.event_id),
                idempotency: None,
                worker: Some(lease),
            })
            .await
            .map_err(|error| match error {
                MatchingStoreError::StaleSnapshot => ConsumerError::StaleSnapshot,
                MatchingStoreError::NotFound
                | MatchingStoreError::EvidenceNotFound
                | MatchingStoreError::IdempotencyConflict
                | MatchingStoreError::TooManyRecords
                | MatchingStoreError::Unavailable => ConsumerError::Unavailable,
            })? {
            PersistedMatch::Evidence {
                evidence_run_id, ..
            } => Ok(ConsumerCompletion::EvidencePersisted { evidence_run_id }),
            PersistedMatch::Obsolete { code } => Ok(ConsumerCompletion::Obsolete { code }),
        }
    }

    async fn evaluate_internal(
        &self,
        request: MatchEvaluationRequest<'_>,
    ) -> Result<PersistedMatch, MatchingStoreError> {
        // Diagnostic durations use one monotonic clock and milliseconds:
        // elapsed wall time, not thread CPU counters or server execution time.
        // Only normal newly persisted successes emit aggregate counts/timings; no
        // IDs, source values, parameters, credentials or payloads are logged.
        let evaluation_started = Instant::now();
        let _admission = self
            .evaluation_admission
            .clone()
            .try_acquire_owned()
            .map_err(|_| MatchingStoreError::Unavailable)?;
        let active_wait_started = Instant::now();
        let _active = tokio::time::timeout(
            Duration::from_secs(30),
            self.evaluation_active.clone().acquire_owned(),
        )
        .await
        .map_err(|_| MatchingStoreError::Unavailable)?
        .map_err(|_| MatchingStoreError::Unavailable)?;
        let active_wait_ms = active_wait_started.elapsed().as_secs_f64() * 1000.0;
        let pre_candidate_started = Instant::now();
        // Load and validate versions only after admission; queued calls do not
        // retain database connections or a stale candidate/source snapshot.
        let MatchEvaluationRequest {
            registry_id,
            organization_id,
            query_source_record_id,
            candidate_limit,
            actor_id,
            request_id,
            idempotency,
            worker,
        } = request;
        if let Some(idempotency) = idempotency {
            let query_is_active: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM litecr.source_record sr \
                 JOIN litecr.source_record_version sv \
                   ON sv.source_record_id=sr.source_record_id AND sv.receive_version=sr.current_receive_version \
                 WHERE sr.source_record_id=$1::uuid AND sr.registry_id=$2 AND sr.organization_id=$3 \
                   AND sr.withdrawn_at IS NULL AND sv.withdrawn_at IS NULL AND sv.version_state='accepted')",
            )
            .bind(query_source_record_id)
            .bind(registry_id)
            .bind(organization_id)
            .fetch_one(&self.pool)
            .await
            .map_err(|_| MatchingStoreError::Unavailable)?;
            if !query_is_active {
                return Err(MatchingStoreError::NotFound);
            }
            if let Some(result) = lookup_idempotent_result(
                &self.pool,
                idempotency,
                registry_id,
                organization_id,
                query_source_record_id,
            )
            .await?
            {
                return Ok(PersistedMatch::Evidence {
                    evidence_run_id: result
                        .evidence_run_id
                        .clone()
                        .ok_or(MatchingStoreError::Unavailable)?,
                    result: Some(Box::new(result)),
                });
            }
        }
        // Build a superset of the pure matcher's blocking passes from current,
        // accepted, versioned keys. Unindexed legacy rows are retained as a safe
        // exhaustive fallback until `litecr matching reindex` has covered them.
        // Tolerant predicates depend only on name keys: evaluate each distinct
        // eligible key once, then expand it to every source (including twins).
        // The lateral UUID lookup prevents a repeated scoped-key scan when a
        // freshly populated database has no useful planner statistics yet.
        // An absent character needs an edit; per-part guards allow one edit
        // and overestimate compatibility before the unchanged exact helpers.
        // Five disjoint suffix trigrams cannot all be disrupted by two OSA
        // edits (a transposition disrupts at most two). A one-edit part prefix
        // preserves one of three query-token trigrams, unless the reverse
        // assignment uses a shorter token; the length bound retains that case.
        // Short/odd queries and fewer-part candidates stay unfiltered. These
        // necessary conditions never replace the exact helper predicates.
        // Final projection also uses UUID-first lookups so refreshed statistics
        // cannot turn selected IDs into whole-scope UUID index walks. Native
        // UUID ordering and final current-version/scope checks remain intact.
        // Key precomputation may include scoped historical keys; only the later
        // eligible-source join can return IDs, preserving current-version and
        // accepted/non-withdrawn checks while avoiding early source-wide joins.
        // JIT compilation costs more than it saves for this bounded interactive
        // query. A read-only transaction confines this setting to the candidate
        // statement; commit/rollback restores the pooled connection's policy.
        let pre_candidate_ms = pre_candidate_started.elapsed().as_secs_f64() * 1000.0;
        let candidate_acquire_started = Instant::now();
        let mut candidate_connection = self
            .pool
            .acquire()
            .await
            .map_err(|_| MatchingStoreError::Unavailable)?;
        let candidate_pool_acquire_ms = candidate_acquire_started.elapsed().as_secs_f64() * 1000.0;
        // Includes BEGIN, local transaction settings, fetch/decode by SQLx and
        // read-transaction COMMIT; excludes the separately timed pool acquire.
        let candidate_read_started = Instant::now();
        let mut candidate_tx = sqlx::Connection::begin(&mut *candidate_connection)
            .await
            .map_err(|_| MatchingStoreError::Unavailable)?;
        sqlx::query("SET TRANSACTION READ ONLY")
            .execute(&mut *candidate_tx)
            .await
            .map_err(|_| MatchingStoreError::Unavailable)?;
        sqlx::query("SET LOCAL jit=off")
            .execute(&mut *candidate_tx)
            .await
            .map_err(|_| MatchingStoreError::Unavailable)?;
        // Exact keys of 4..=128 characters are already returned through the
        // tolerant equality branch with the same current/scope predicates.
        // Keep the indexed exact pass outside that range: e.g. one Lao code
        // point has multiple UTF-8 bytes, and long exact names remain valid.
        let rows=sqlx::query(
            "WITH query_source AS ( \
               SELECT sr.source_record_id,sr.person_id,sr.current_receive_version \
               FROM litecr.source_record sr JOIN litecr.source_record_version qv \
                 ON qv.source_record_id=sr.source_record_id AND qv.receive_version=sr.current_receive_version \
               WHERE sr.source_record_id=$3::uuid AND sr.registry_id=$1 AND sr.organization_id=$2 \
                 AND sr.withdrawn_at IS NULL AND qv.withdrawn_at IS NULL AND qv.version_state='accepted' \
             ), indexed_candidates AS ( \
               SELECT DISTINCT ca.source_record_id \
               FROM query_source qs \
               JOIN matching.normalized_assertion qa ON qa.source_record_id=qs.source_record_id \
                 AND qa.receive_version=qs.current_receive_version AND qa.normalizer_version=$4 \
                 AND qa.representation='source_original' AND lower(qa.field_path) IN ('name','name.original') \
                 AND octet_length(qa.normalized_key)>=2 \
                 AND char_length(qa.normalized_key) NOT BETWEEN 4 AND 128 \
               JOIN matching.normalized_assertion ca ON ca.registry_id=$1 AND ca.organization_id=$2 \
                 AND ca.normalizer_version=qa.normalizer_version AND ca.representation='source_original' \
                 AND lower(ca.field_path) IN ('name','name.original') AND octet_length(ca.normalized_key)>=2 \
                 AND md5(ca.normalized_key)=md5(qa.normalized_key) AND ca.normalized_key=qa.normalized_key \
               JOIN litecr.source_record sr ON sr.source_record_id=ca.source_record_id \
                 AND sr.current_receive_version=ca.receive_version AND sr.registry_id=$1 AND sr.organization_id=$2 \
                 AND sr.withdrawn_at IS NULL AND sr.person_id<>qs.person_id \
               JOIN litecr.source_record_version sv ON sv.source_record_id=sr.source_record_id \
                 AND sv.receive_version=sr.current_receive_version AND sv.withdrawn_at IS NULL AND sv.version_state='accepted' \
               UNION \
               SELECT DISTINCT ca.source_record_id \
               FROM query_source qs \
               JOIN matching.normalized_assertion qa ON qa.source_record_id=qs.source_record_id \
                 AND qa.receive_version=qs.current_receive_version AND qa.normalizer_version=$4 \
                 AND lower(qa.field_path) IN ('birth_date','date_of_birth') \
                 AND length(qa.normalized_key)=4 AND qa.normalized_key ~ '^[0-9]{4}$' \
               JOIN matching.normalized_assertion ca ON ca.registry_id=$1 AND ca.organization_id=$2 \
                 AND ca.normalizer_version=qa.normalizer_version \
                 AND lower(ca.field_path) IN ('birth_date','date_of_birth') \
                 AND length(ca.normalized_key)=4 AND ca.normalized_key ~ '^[0-9]{4}$' \
                 AND md5(ca.normalized_key)=md5(qa.normalized_key) AND ca.normalized_key=qa.normalized_key \
               JOIN litecr.source_record sr ON sr.source_record_id=ca.source_record_id \
                 AND sr.current_receive_version=ca.receive_version AND sr.registry_id=$1 AND sr.organization_id=$2 \
                 AND sr.withdrawn_at IS NULL AND sr.person_id<>qs.person_id \
               JOIN litecr.source_record_version sv ON sv.source_record_id=sr.source_record_id \
                 AND sv.receive_version=sr.current_receive_version AND sv.withdrawn_at IS NULL AND sv.version_state='accepted' \
               UNION \
               SELECT DISTINCT ci.source_record_id \
               FROM query_source qs \
               JOIN matching.normalized_identifier qi ON qi.source_record_id=qs.source_record_id \
                 AND qi.receive_version=qs.current_receive_version AND qi.normalizer_version=$4 \
               JOIN matching.normalized_identifier ci ON ci.registry_id=$1 AND ci.organization_id=$2 \
                 AND ci.authority=qi.authority AND ci.identifier_type=qi.identifier_type \
                 AND ci.normalizer_version=qi.normalizer_version \
                 AND md5(ci.normalized_key)=md5(qi.normalized_key) AND ci.normalized_key=qi.normalized_key \
               JOIN litecr.source_identifier_assertion qia ON qia.source_record_id=qi.source_record_id \
                 AND qia.receive_version=qi.receive_version AND qia.authority=qi.authority \
                 AND qia.identifier_type=qi.identifier_type AND qia.authoritative=TRUE \
               JOIN litecr.source_identifier_assertion cia ON cia.source_record_id=ci.source_record_id \
                 AND cia.receive_version=ci.receive_version AND cia.authority=ci.authority \
                 AND cia.identifier_type=ci.identifier_type AND cia.authoritative=TRUE \
               JOIN litecr.source_record sr ON sr.source_record_id=ci.source_record_id \
                 AND sr.current_receive_version=ci.receive_version AND sr.registry_id=$1 AND sr.organization_id=$2 \
                 AND sr.withdrawn_at IS NULL AND sr.person_id<>qs.person_id \
               JOIN litecr.source_record_version sv ON sv.source_record_id=sr.source_record_id \
                 AND sv.receive_version=sr.current_receive_version AND sv.withdrawn_at IS NULL AND sv.version_state='accepted' \
             ), legacy_candidates AS ( \
               SELECT sr.source_record_id \
               FROM query_source qs JOIN litecr.source_record sr ON sr.registry_id=$1 AND sr.organization_id=$2 \
                 AND sr.withdrawn_at IS NULL AND sr.person_id<>qs.person_id \
               JOIN litecr.source_record_version sv ON sv.source_record_id=sr.source_record_id \
                 AND sv.receive_version=sr.current_receive_version AND sv.withdrawn_at IS NULL AND sv.version_state='accepted' \
               WHERE NOT EXISTS (SELECT 1 FROM matching.indexed_source_version qix \
                 WHERE qix.source_record_id=qs.source_record_id AND qix.receive_version=qs.current_receive_version \
                   AND qix.normalizer_version=$4) \
                  OR NOT EXISTS (SELECT 1 FROM matching.indexed_source_version ix \
                    WHERE ix.source_record_id=sr.source_record_id AND ix.receive_version=sr.current_receive_version \
                      AND ix.normalizer_version=$4) \
             ), query_name_keys AS MATERIALIZED ( \
               SELECT DISTINCT qa.normalized_key,char_length(qa.normalized_key) AS name_length, \
                 cardinality(string_to_array(qa.normalized_key,' ')) AS part_count, \
                 (SELECT part.value FROM unnest(string_to_array(qa.normalized_key,' ')) part(value) \
                  ORDER BY char_length(part.value) DESC,part.value LIMIT 1) AS longest_part \
               FROM query_source qs \
               JOIN matching.normalized_assertion qa ON qa.source_record_id=qs.source_record_id \
                 AND qa.receive_version=qs.current_receive_version AND qa.normalizer_version=$4 \
                 AND qa.representation='source_original' AND lower(qa.field_path) IN ('name','name.original') \
                 AND char_length(qa.normalized_key) BETWEEN 4 AND 128 \
             ), candidate_name_keys AS MATERIALIZED ( \
               SELECT DISTINCT ca.normalized_key FROM matching.normalized_assertion ca \
               WHERE ca.registry_id=$1 AND ca.organization_id=$2 AND ca.normalizer_version=$4 \
                 AND ca.representation='source_original' AND lower(ca.field_path) IN ('name','name.original') \
                 AND char_length(ca.normalized_key) BETWEEN 4 AND 128 \
             ), tolerant_name_keys AS MATERIALIZED ( \
               SELECT ca.normalized_key FROM candidate_name_keys ca \
               WHERE EXISTS (SELECT 1 FROM query_name_keys qa \
                 WHERE CASE WHEN /* lossless_prefilter_begin */ \
                   qa.name_length<15 OR char_length(qa.longest_part)<9 OR qa.part_count NOT BETWEEN 1 AND 8 \
                   OR position(substr(right(qa.normalized_key,15),1,3) in ca.normalized_key)>0 \
                   OR position(substr(right(qa.normalized_key,15),4,3) in ca.normalized_key)>0 \
                   OR position(substr(right(qa.normalized_key,15),7,3) in ca.normalized_key)>0 \
                   OR position(substr(right(qa.normalized_key,15),10,3) in ca.normalized_key)>0 \
                   OR position(substr(right(qa.normalized_key,15),13,3) in ca.normalized_key)>0 \
                   OR position(substr(qa.longest_part,1,3) in ca.normalized_key)>0 \
                   OR position(substr(qa.longest_part,4,3) in ca.normalized_key)>0 \
                   OR position(substr(qa.longest_part,7,3) in ca.normalized_key)>0 \
                   OR cardinality(string_to_array(ca.normalized_key,' '))<qa.part_count \
                   OR (cardinality(string_to_array(ca.normalized_key,' '))=qa.part_count \
                     AND char_length(ca.normalized_key)<=qa.name_length-char_length(qa.longest_part)+8+(qa.part_count-1)) \
                   /* lossless_prefilter_end */ THEN CASE WHEN qa.normalized_key=ca.normalized_key THEN TRUE ELSE \
                   CASE WHEN char_length(translate(qa.normalized_key,ca.normalized_key,''))<=(CASE WHEN least(char_length(qa.normalized_key),char_length(ca.normalized_key))>=8 THEN 2 ELSE 1 END) \
                     AND char_length(translate(ca.normalized_key,qa.normalized_key,''))<=(CASE WHEN least(char_length(qa.normalized_key),char_length(ca.normalized_key))>=8 THEN 2 ELSE 1 END) \
                   THEN matching.name_edit_within_v1(qa.normalized_key,ca.normalized_key,CASE WHEN least(char_length(qa.normalized_key),char_length(ca.normalized_key))>=8 THEN 2 ELSE 1 END) ELSE FALSE END \
                   OR CASE WHEN NOT EXISTS (SELECT 1 FROM unnest(string_to_array(qa.normalized_key,' ')) sp(value) \
                     WHERE char_length(sp.value)<3 OR NOT EXISTS ( \
                       SELECT 1 FROM unnest(string_to_array(ca.normalized_key,' ')) fp(value) \
                       WHERE char_length(fp.value)>=greatest(3,char_length(sp.value)-1) \
                         AND char_length(translate(sp.value,left(fp.value,char_length(sp.value)+1),''))<=1)) \
                   THEN matching.name_parts_within_v1(qa.normalized_key,ca.normalized_key) ELSE FALSE END \
                   OR CASE WHEN NOT EXISTS (SELECT 1 FROM unnest(string_to_array(ca.normalized_key,' ')) sp(value) \
                     WHERE char_length(sp.value)<3 OR NOT EXISTS ( \
                       SELECT 1 FROM unnest(string_to_array(qa.normalized_key,' ')) fp(value) \
                       WHERE char_length(fp.value)>=greatest(3,char_length(sp.value)-1) \
                         AND char_length(translate(sp.value,left(fp.value,char_length(sp.value)+1),''))<=1)) \
                   THEN matching.name_parts_within_v1(ca.normalized_key,qa.normalized_key) ELSE FALSE END END ELSE FALSE END) \
             ), eligible_name_assertions AS MATERIALIZED ( \
               SELECT ca.source_record_id,ca.normalized_key \
               FROM query_source qs JOIN tolerant_name_keys keys ON TRUE \
               JOIN matching.normalized_assertion ca ON ca.registry_id=$1 AND ca.organization_id=$2 \
                 AND md5(ca.normalized_key)=md5(keys.normalized_key) AND ca.normalized_key=keys.normalized_key \
                 AND ca.normalizer_version=$4 AND ca.representation='source_original' \
                 AND lower(ca.field_path) IN ('name','name.original') \
                 AND char_length(ca.normalized_key) BETWEEN 4 AND 128 \
               JOIN LATERAL (SELECT s.person_id,s.current_receive_version,s.registry_id,s.organization_id,s.withdrawn_at \
                 FROM litecr.source_record s WHERE s.source_record_id=ca.source_record_id OFFSET 0) sr \
                 ON sr.current_receive_version=ca.receive_version AND sr.registry_id=$1 AND sr.organization_id=$2 \
                 AND sr.withdrawn_at IS NULL AND sr.person_id<>qs.person_id \
               JOIN litecr.source_record_version sv ON sv.source_record_id=ca.source_record_id \
                 AND sv.receive_version=sr.current_receive_version AND sv.withdrawn_at IS NULL AND sv.version_state='accepted' \
             ), tolerant_name_candidates AS ( \
               SELECT DISTINCT ca.source_record_id FROM eligible_name_assertions ca \
             ), candidate_ids AS ( \
               SELECT source_record_id FROM indexed_candidates \
               UNION SELECT source_record_id FROM legacy_candidates \
               UNION SELECT source_record_id FROM tolerant_name_candidates \
             ), selected_ids AS ( \
               SELECT source_record_id FROM query_source UNION SELECT source_record_id FROM candidate_ids \
             ) \
             SELECT sr.source_record_id::text AS record_id,sr.registry_id,sr.organization_id,sv.source_version,sv.receive_version, \
               COALESCE((SELECT jsonb_agg(jsonb_build_object('field_path',a.field_path,'original_value',a.original_value,'language_tag',a.language_tag,'script',a.script) ORDER BY a.field_path) \
                 FROM litecr.source_assertion a WHERE a.source_record_id=sr.source_record_id AND a.receive_version=sv.receive_version AND a.withdrawn_at IS NULL),'[]'::jsonb) AS assertions, \
               COALESCE((SELECT jsonb_agg(jsonb_build_object('authority',i.authority,'identifier_type',i.identifier_type,'value',i.original_value,'authoritative',i.authoritative,'status',i.assertion_state) ORDER BY i.authority,i.identifier_type) \
                 FROM litecr.source_identifier_assertion i WHERE i.source_record_id=sr.source_record_id AND i.receive_version=sv.receive_version AND i.withdrawn_at IS NULL),'[]'::jsonb) AS identifiers \
             FROM selected_ids ids \
             JOIN LATERAL (SELECT s.source_record_id,s.registry_id,s.organization_id,s.current_receive_version,s.withdrawn_at \
               FROM litecr.source_record s WHERE s.source_record_id=ids.source_record_id OFFSET 0) sr ON TRUE \
             JOIN LATERAL (SELECT v.source_record_id,v.source_version,v.receive_version,v.withdrawn_at,v.version_state \
               FROM litecr.source_record_version v WHERE v.source_record_id=sr.source_record_id \
                 AND v.receive_version=sr.current_receive_version OFFSET 0) sv ON TRUE \
             WHERE sr.registry_id=$1 AND sr.organization_id=$2 AND sr.withdrawn_at IS NULL \
               AND sv.withdrawn_at IS NULL AND sv.version_state='accepted' \
             ORDER BY sr.source_record_id LIMIT 10001"
        ).bind(registry_id).bind(organization_id).bind(query_source_record_id).bind(NORMALIZATION_VERSION).fetch_all(&mut *candidate_tx).await.map_err(|_|MatchingStoreError::Unavailable)?;
        candidate_tx
            .commit()
            .await
            .map_err(|_| MatchingStoreError::Unavailable)?;
        drop(candidate_connection);
        let candidate_read_ms = candidate_read_started.elapsed().as_secs_f64() * 1000.0;
        let decode_started = Instant::now();
        let retrieved_rows = rows.len();
        if rows.is_empty() {
            if let Some(worker) = worker {
                return self
                    .finish_obsolete(worker, ConsumerReceiptCode::SourceWithdrawn)
                    .await;
            }
            return Err(MatchingStoreError::NotFound);
        }
        if rows.len() > 10_000 {
            return Err(MatchingStoreError::TooManyRecords);
        }
        let mut records = Vec::with_capacity(rows.len());
        let mut snapshot = Sha256::new();
        for row in rows {
            let assertions_json: Value = row
                .try_get("assertions")
                .map_err(|_| MatchingStoreError::Unavailable)?;
            let ids_json: Value = row
                .try_get("identifiers")
                .map_err(|_| MatchingStoreError::Unavailable)?;
            let assertions: Vec<MatchAssertion> = serde_json::from_value(assertions_json)
                .map_err(|_| MatchingStoreError::Unavailable)?;
            let identifiers: Vec<MatchIdentifier> =
                serde_json::from_value(ids_json).map_err(|_| MatchingStoreError::Unavailable)?;
            let record_id: String = row
                .try_get("record_id")
                .map_err(|_| MatchingStoreError::Unavailable)?;
            let reg: String = row
                .try_get("registry_id")
                .map_err(|_| MatchingStoreError::Unavailable)?;
            let org: String = row
                .try_get("organization_id")
                .map_err(|_| MatchingStoreError::Unavailable)?;
            let source_version: Option<String> = row
                .try_get("source_version")
                .map_err(|_| MatchingStoreError::Unavailable)?;
            let receive_version: i64 = row
                .try_get("receive_version")
                .map_err(|_| MatchingStoreError::Unavailable)?;
            snapshot.update(record_id.as_bytes());
            snapshot.update(receive_version.to_be_bytes());
            snapshot.update(source_version.as_deref().unwrap_or("").as_bytes());
            for assertion in &assertions {
                snapshot.update((assertion.field_path.len() as u64).to_be_bytes());
                snapshot.update(assertion.field_path.as_bytes());
                snapshot.update((assertion.original_value.len() as u64).to_be_bytes());
                snapshot.update(assertion.original_value.as_bytes());
            }
            let names = assertions
                .iter()
                .filter(|a| is_matchable_original_name_field_path(&a.field_path))
                .cloned()
                .collect();
            let date = assertions
                .iter()
                .find(|a| {
                    a.field_path.eq_ignore_ascii_case("birth_date")
                        || a.field_path.eq_ignore_ascii_case("date_of_birth")
                })
                .map(|a| PartialDate {
                    original_value: a.original_value.clone(),
                    precision: date_precision(&a.original_value),
                    calendar: None,
                    estimated: false,
                });
            records.push(MatchRecord {
                record_id,
                registry_id: reg,
                organization_id: org,
                source_version,
                receive_version: Some(receive_version),
                name_assertions: names,
                assertions,
                date_of_birth: date,
                identifiers,
                contacts: vec![],
                address_token: None,
            });
        }
        let query_version = records
            .iter()
            .find(|r| r.record_id == query_source_record_id)
            .and_then(|r| r.receive_version)
            .ok_or(MatchingStoreError::NotFound)?;
        let query_id = query_source_record_id.to_owned();
        let decode_snapshot_ms = decode_started.elapsed().as_secs_f64() * 1000.0;
        let blocking_enqueued = Instant::now();
        let (matched, blocking_started, blocking_finished, cleanup_finished) =
            tokio::task::spawn_blocking(move || {
                let started = Instant::now();
                let matched = match_records(&query_id, &records, candidate_limit);
                let finished = Instant::now();
                // Free the loaded cohort on the blocking thread, and measure
                // that separately from matching and completion/resume delay.
                drop(records);
                drop(query_id);
                (matched, started, finished, Instant::now())
            })
            .await
            .map_err(|_| MatchingStoreError::Unavailable)?;
        let blocking_queue_ms = blocking_started
            .duration_since(blocking_enqueued)
            .as_secs_f64()
            * 1000.0;
        let pure_match_ms = blocking_finished
            .duration_since(blocking_started)
            .as_secs_f64()
            * 1000.0;
        let blocking_cleanup_ms = cleanup_finished
            .duration_since(blocking_finished)
            .as_secs_f64()
            * 1000.0;
        let blocking_resume_ms = cleanup_finished.elapsed().as_secs_f64() * 1000.0;
        let post_match_started = Instant::now();
        let mut result = matched.map_err(|_| MatchingStoreError::Unavailable)?;
        let digest: Vec<u8> = snapshot.finalize().to_vec();
        let post_match_prepare_ms = post_match_started.elapsed().as_secs_f64() * 1000.0;
        let persist_acquire_started = Instant::now();
        let mut persist_connection = self
            .pool
            .acquire()
            .await
            .map_err(|_| MatchingStoreError::Unavailable)?;
        let persist_pool_acquire_ms = persist_acquire_started.elapsed().as_secs_f64() * 1000.0;
        let persist_begin_started = Instant::now();
        let mut tx = sqlx::Connection::begin(&mut *persist_connection)
            .await
            .map_err(|_| MatchingStoreError::Unavailable)?;
        let persist_begin_ms = persist_begin_started.elapsed().as_secs_f64() * 1000.0;
        let persist_prepare_started = Instant::now();

        if let Some(idempotency) = idempotency {
            let lock_key = idempotency_lock_key(
                idempotency,
                registry_id,
                organization_id,
                query_source_record_id,
            );
            sqlx::query("SELECT pg_advisory_xact_lock($1::bigint)")
                .bind(lock_key)
                .execute(&mut *tx)
                .await
                .map_err(|_| MatchingStoreError::Unavailable)?;
            if let Some(result) = lookup_idempotent_result(
                &mut *tx,
                idempotency,
                registry_id,
                organization_id,
                query_source_record_id,
            )
            .await?
            {
                tx.commit()
                    .await
                    .map_err(|_| MatchingStoreError::Unavailable)?;
                return Ok(PersistedMatch::Evidence {
                    evidence_run_id: result
                        .evidence_run_id
                        .clone()
                        .ok_or(MatchingStoreError::Unavailable)?,
                    result: Some(Box::new(result)),
                });
            }
        }

        if let Some(worker) = worker {
            if query_version != worker.expected_receive_version {
                self.assert_worker_lease(&mut tx, worker).await?;
                insert_obsolete_receipt(&mut tx, worker, ConsumerReceiptCode::SourceSuperseded)
                    .await?;
                tx.commit()
                    .await
                    .map_err(|_| MatchingStoreError::Unavailable)?;
                return Ok(PersistedMatch::Obsolete {
                    code: ConsumerReceiptCode::SourceSuperseded,
                });
            }
        }

        if let Some(worker) = worker {
            let job = sqlx::query(
                "SELECT j.state,j.lease_owner,j.lease_token::text AS lease_token, \
                        COALESCE(j.lease_expires_at > now(),FALSE) AS lease_current,j.registry_id,j.organization_id, \
                        j.source_record_id::text AS source_record_id,j.expected_receive_version, \
                        o.event_type,o.schema_version,o.aggregate_type,o.aggregate_id::text AS aggregate_id, \
                        o.aggregate_version,o.registry_id AS event_registry,o.organization_id AS event_org \
                 FROM litecr.consumer_job j JOIN litecr.outbox_event o USING(event_id) \
                 WHERE j.consumer_name='advisory-matcher-v1' AND j.event_id=$1::uuid FOR UPDATE OF j",
            )
            .bind(&worker.event_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|_| MatchingStoreError::Unavailable)?
            .ok_or(MatchingStoreError::Unavailable)?;
            let state: String = job
                .try_get("state")
                .map_err(|_| MatchingStoreError::Unavailable)?;
            let owner: Option<String> = job
                .try_get("lease_owner")
                .map_err(|_| MatchingStoreError::Unavailable)?;
            let token: Option<String> = job
                .try_get("lease_token")
                .map_err(|_| MatchingStoreError::Unavailable)?;
            let lease_current: bool = job
                .try_get("lease_current")
                .map_err(|_| MatchingStoreError::Unavailable)?;
            if state != "leased"
                || owner.as_deref() != Some(worker.lease_owner.as_str())
                || token.as_deref() != Some(worker.lease_token.as_str())
                || !lease_current
            {
                if let Some(receipt) = sqlx::query(
                    "SELECT outcome,evidence_run_id::text AS evidence_run_id,receipt_code \
                     FROM litecr.consumer_receipt WHERE consumer_name='advisory-matcher-v1' AND event_id=$1::uuid",
                )
                .bind(&worker.event_id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(|_| MatchingStoreError::Unavailable)?
                {
                    let outcome: String = receipt.try_get("outcome").map_err(|_| MatchingStoreError::Unavailable)?;
                    let evidence_run_id: Option<String> = receipt.try_get("evidence_run_id").map_err(|_| MatchingStoreError::Unavailable)?;
                    if outcome == "completed" {
                        tx.commit().await.map_err(|_| MatchingStoreError::Unavailable)?;
                        return Ok(PersistedMatch::Evidence { result: None, evidence_run_id: evidence_run_id.ok_or(MatchingStoreError::Unavailable)? });
                    }
                    if outcome == "obsolete" {
                        let code: String = receipt.try_get("receipt_code").map_err(|_| MatchingStoreError::Unavailable)?;
                        tx.commit().await.map_err(|_| MatchingStoreError::Unavailable)?;
                        return Ok(PersistedMatch::Obsolete { code: receipt_code_from_str(&code) });
                    }
                }
                return Err(MatchingStoreError::Unavailable);
            }
            let job_registry: String = job
                .try_get("registry_id")
                .map_err(|_| MatchingStoreError::Unavailable)?;
            let job_org: String = job
                .try_get("organization_id")
                .map_err(|_| MatchingStoreError::Unavailable)?;
            let job_source: String = job
                .try_get("source_record_id")
                .map_err(|_| MatchingStoreError::Unavailable)?;
            let job_version: i64 = job
                .try_get("expected_receive_version")
                .map_err(|_| MatchingStoreError::Unavailable)?;
            let event_type: String = job
                .try_get("event_type")
                .map_err(|_| MatchingStoreError::Unavailable)?;
            let schema_version: i32 = job
                .try_get("schema_version")
                .map_err(|_| MatchingStoreError::Unavailable)?;
            let aggregate_type: String = job
                .try_get("aggregate_type")
                .map_err(|_| MatchingStoreError::Unavailable)?;
            let aggregate_id: String = job
                .try_get("aggregate_id")
                .map_err(|_| MatchingStoreError::Unavailable)?;
            let aggregate_version: i64 = job
                .try_get("aggregate_version")
                .map_err(|_| MatchingStoreError::Unavailable)?;
            let event_registry: String = job
                .try_get("event_registry")
                .map_err(|_| MatchingStoreError::Unavailable)?;
            let event_org: String = job
                .try_get("event_org")
                .map_err(|_| MatchingStoreError::Unavailable)?;
            if job_registry != worker.registry_id
                || job_org != worker.organization_id
                || job_source != worker.source_record_id
                || job_version != worker.expected_receive_version
                || job_registry != event_registry
                || job_org != event_org
                || job_source != aggregate_id
                || job_version != aggregate_version
                || event_type != "registry.source_record.accepted"
                || schema_version != 1
                || aggregate_type != "source_record"
            {
                insert_obsolete_receipt(&mut tx, worker, ConsumerReceiptCode::SourceScopeMismatch)
                    .await?;
                tx.commit()
                    .await
                    .map_err(|_| MatchingStoreError::Unavailable)?;
                return Ok(PersistedMatch::Obsolete {
                    code: ConsumerReceiptCode::SourceScopeMismatch,
                });
            }
        }

        // Scoring happens outside the transaction. Before persisting its result,
        // lock and re-read the query source plus every candidate returned to the
        // caller. Writers that change or withdraw any of these rows serialize
        // with this check, so the evidence cannot commit against a stale version.
        let mut expected_versions = BTreeMap::new();
        expected_versions.insert(query_source_record_id.to_owned(), query_version);
        for candidate in &result.candidates {
            let expected_version = candidate
                .candidate_receive_version
                .ok_or(MatchingStoreError::StaleSnapshot)?;
            if expected_versions
                .insert(candidate.candidate_record_id.clone(), expected_version)
                .is_some_and(|previous| previous != expected_version)
            {
                return Err(MatchingStoreError::StaleSnapshot);
            }
        }
        let snapshot_ids: Vec<String> = expected_versions.keys().cloned().collect();
        let persist_prepare_ms = persist_prepare_started.elapsed().as_secs_f64() * 1000.0;
        let snapshot_lock_started = Instant::now();
        let locked_rows = if worker.is_some() {
            // The migration-owned SECURITY DEFINER function holds the same
            // source/version locks without granting the worker UPDATE on facts.
            sqlx::query(
                "SELECT source_record_id::text AS source_record_id, current_receive_version, \
                        version_state, source_active, version_active \
                 FROM matching.lock_advisory_source_snapshot($1,$2,$3::text[])",
            )
            .bind(registry_id)
            .bind(organization_id)
            .bind(&snapshot_ids)
            .fetch_all(&mut *tx)
            .await
            .map_err(|_| MatchingStoreError::Unavailable)?
        } else {
            sqlx::query(
                "SELECT sr.source_record_id::text AS source_record_id, \
                        sr.current_receive_version, sv.version_state, \
                        (sr.withdrawn_at IS NULL) AS source_active, \
                        (sv.withdrawn_at IS NULL) AS version_active \
                 FROM litecr.source_record sr \
                 JOIN litecr.source_record_version sv \
                   ON sv.source_record_id = sr.source_record_id \
                  AND sv.receive_version = sr.current_receive_version \
                 WHERE sr.source_record_id::text = ANY($3::text[]) \
                   AND sr.registry_id = $1 AND sr.organization_id = $2 \
                 ORDER BY sr.source_record_id \
                 FOR UPDATE OF sr, sv",
            )
            .bind(registry_id)
            .bind(organization_id)
            .bind(&snapshot_ids)
            .fetch_all(&mut *tx)
            .await
            .map_err(|_| MatchingStoreError::Unavailable)?
        };
        // Client elapsed for the snapshot-lock SELECT, including actual lock
        // waiting and row transfer; it is not pure PostgreSQL CPU time.
        let snapshot_lock_ms = snapshot_lock_started.elapsed().as_secs_f64() * 1000.0;
        let snapshot_validate_started = Instant::now();
        if locked_rows.len() != expected_versions.len() {
            if let Some(worker) = worker {
                let query_present = locked_rows.iter().any(|row| {
                    row.try_get::<String, _>("source_record_id").ok().as_deref()
                        == Some(worker.source_record_id.as_str())
                });
                if !query_present {
                    insert_obsolete_receipt(
                        &mut tx,
                        worker,
                        ConsumerReceiptCode::SourceScopeMismatch,
                    )
                    .await?;
                    tx.commit()
                        .await
                        .map_err(|_| MatchingStoreError::Unavailable)?;
                    return Ok(PersistedMatch::Obsolete {
                        code: ConsumerReceiptCode::SourceScopeMismatch,
                    });
                }
            }
            return Err(MatchingStoreError::StaleSnapshot);
        }
        for row in locked_rows {
            let source_record_id: String = row
                .try_get("source_record_id")
                .map_err(|_| MatchingStoreError::Unavailable)?;
            let current_version: i64 = row
                .try_get("current_receive_version")
                .map_err(|_| MatchingStoreError::Unavailable)?;
            let version_state: String = row
                .try_get("version_state")
                .map_err(|_| MatchingStoreError::Unavailable)?;
            let source_active: bool = row
                .try_get("source_active")
                .map_err(|_| MatchingStoreError::Unavailable)?;
            let version_active: bool = row
                .try_get("version_active")
                .map_err(|_| MatchingStoreError::Unavailable)?;
            if expected_versions.get(&source_record_id) != Some(&current_version)
                || version_state != "accepted"
                || !source_active
                || !version_active
            {
                if let Some(worker) = worker {
                    if source_record_id == worker.source_record_id {
                        let code = if !source_active || !version_active {
                            ConsumerReceiptCode::SourceWithdrawn
                        } else {
                            ConsumerReceiptCode::SourceSuperseded
                        };
                        insert_obsolete_receipt(&mut tx, worker, code).await?;
                        tx.commit()
                            .await
                            .map_err(|_| MatchingStoreError::Unavailable)?;
                        return Ok(PersistedMatch::Obsolete { code });
                    }
                }
                return Err(MatchingStoreError::StaleSnapshot);
            }
        }

        let snapshot_validate_ms = snapshot_validate_started.elapsed().as_secs_f64() * 1000.0;
        let evidence_write_started = Instant::now();
        // Allocate the ID before serialization so the complete historical
        // response is inserted once, without requiring snapshot UPDATE rights.
        let run_id: String = sqlx::query_scalar("SELECT gen_random_uuid()::text")
            .fetch_one(&mut *tx)
            .await
            .map_err(|_| MatchingStoreError::Unavailable)?;
        result.evidence_run_id = Some(run_id.clone());
        result.snapshot_complete = true;
        let snapshot =
            serde_json::to_value(&result).map_err(|_| MatchingStoreError::Unavailable)?;
        sqlx::query(
            "INSERT INTO matching.evidence_run(registry_id,organization_id,query_source_record_id,query_receive_version,query_source_version,engine_version,normalization_version,candidate_version,feature_version,policy_version,source_snapshot_digest,candidate_count,candidates_truncated,actor_id,request_id,run_id,result_snapshot) \
             VALUES($1,$2,$3::uuid,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16::uuid,$17)"
        ).bind(registry_id).bind(organization_id).bind(query_source_record_id).bind(query_version).bind(result.query_source_version.as_deref()).bind(&result.versions.engine).bind(&result.versions.normalization).bind(&result.versions.candidate).bind(&result.versions.feature).bind(&result.versions.policy).bind(digest).bind(result.candidates.len() as i32).bind(result.retrieval.truncated).bind(actor_id).bind(request_id).bind(&run_id).bind(snapshot).execute(&mut *tx).await.map_err(|_|MatchingStoreError::Unavailable)?;
        if let Some(idempotency) = idempotency {
            sqlx::query(
                "INSERT INTO matching.request_idempotency \
                 (actor_digest,registry_id,organization_id,query_source_record_id,key_digest,request_digest,evidence_run_id) \
                 VALUES($1,$2,$3,$4::uuid,$5,$6,$7::uuid)",
            )
            .bind(idempotency.actor_digest.to_vec())
            .bind(registry_id)
            .bind(organization_id)
            .bind(query_source_record_id)
            .bind(idempotency.key_digest.to_vec())
            .bind(idempotency.request_digest.to_vec())
            .bind(&run_id)
            .execute(&mut *tx)
            .await
            .map_err(|_| MatchingStoreError::Unavailable)?;
        }
        for candidate in &result.candidates {
            let encoded =
                serde_json::to_value(candidate).map_err(|_| MatchingStoreError::Unavailable)?;
            sqlx::query("INSERT INTO matching.evidence_candidate(run_id,candidate_source_record_id,candidate_receive_version,candidate_source_version,score_points,score_type,advisory_outcome,evidence) VALUES($1::uuid,$2::uuid,$3,$4,$5,$6,$7,$8)")
                .bind(&run_id).bind(&candidate.candidate_record_id).bind(candidate.candidate_receive_version.unwrap_or_default()).bind(candidate.candidate_source_version.as_deref()).bind(candidate.score_points).bind(&candidate.score_type).bind(match candidate.outcome {litecr_matching::AdvisoryOutcome::Review=>"review",litecr_matching::AdvisoryOutcome::InsufficientEvidence=>"insufficient_evidence",litecr_matching::AdvisoryOutcome::ConflictReview=>"conflict_review"}).bind(encoded).execute(&mut *tx).await.map_err(|_|MatchingStoreError::Unavailable)?;
        }
        if let Some(worker) = worker {
            sqlx::query(
                "INSERT INTO litecr.consumer_receipt(consumer_name,event_id,outcome,evidence_run_id) \
                 VALUES('advisory-matcher-v1',$1::uuid,'completed',$2::uuid) \
                 ON CONFLICT (consumer_name,event_id) DO NOTHING",
            )
            .bind(&worker.event_id)
            .bind(&run_id)
            .execute(&mut *tx)
            .await
            .map_err(|_| MatchingStoreError::Unavailable)?;
            sqlx::query(
                "UPDATE litecr.consumer_job SET state='completed',lease_owner=NULL,lease_token=NULL,lease_expires_at=NULL, \
                 updated_at=now(),last_error_code=NULL WHERE consumer_name='advisory-matcher-v1' \
                 AND event_id=$1::uuid AND state='leased' AND lease_owner=$2 AND lease_token=$3::uuid",
            )
            .bind(&worker.event_id)
            .bind(&worker.lease_owner)
            .bind(&worker.lease_token)
            .execute(&mut *tx)
            .await
            .map_err(|_| MatchingStoreError::Unavailable)?;
        }
        // Write phase includes UUID allocation, JSON encoding, evidence and
        // optional idempotency/worker writes, but excludes COMMIT below.
        let evidence_write_ms = evidence_write_started.elapsed().as_secs_f64() * 1000.0;
        let persist_commit_started = Instant::now();
        tx.commit()
            .await
            .map_err(|_| MatchingStoreError::Unavailable)?;
        drop(persist_connection);
        let persist_commit_ms = persist_commit_started.elapsed().as_secs_f64() * 1000.0;
        tracing::debug!(
            target: "litecr::matching::performance",
            profile_json = %serde_json::json!({
                "active_wait_ms": active_wait_ms,
                "pre_candidate_ms": pre_candidate_ms,
                "candidate_pool_acquire_ms": candidate_pool_acquire_ms,
                "candidate_read_ms": candidate_read_ms,
                "decode_snapshot_ms": decode_snapshot_ms,
                "blocking_queue_ms": blocking_queue_ms,
                "pure_match_ms": pure_match_ms,
                "blocking_cleanup_ms": blocking_cleanup_ms,
                "blocking_resume_ms": blocking_resume_ms,
                "post_match_prepare_ms": post_match_prepare_ms,
                "persist_pool_acquire_ms": persist_pool_acquire_ms,
                "persist_begin_ms": persist_begin_ms,
                "persist_prepare_ms": persist_prepare_ms,
                "snapshot_lock_ms": snapshot_lock_ms,
                "snapshot_validate_ms": snapshot_validate_ms,
                "evidence_write_ms": evidence_write_ms,
                "persist_commit_ms": persist_commit_ms,
                "total_ms": evaluation_started.elapsed().as_secs_f64() * 1000.0,
                "retrieved_rows": retrieved_rows,
                "union_before_limit": result.retrieval.union_before_limit,
                "returned_candidates": result.candidates.len()
            }),
            "matching_stage_profile"
        );
        Ok(PersistedMatch::Evidence {
            result: Some(Box::new(result)),
            evidence_run_id: run_id,
        })
    }

    async fn finish_obsolete(
        &self,
        worker: &ConsumerLease,
        code: ConsumerReceiptCode,
    ) -> Result<PersistedMatch, MatchingStoreError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|_| MatchingStoreError::Unavailable)?;
        self.assert_worker_lease(&mut tx, worker).await?;
        let current = sqlx::query(
            "SELECT $1::text AS registry_id,$2::text AS organization_id, \
                    current_receive_version,source_active,version_state,version_active \
             FROM matching.lock_advisory_source_snapshot($1,$2,ARRAY[$3::text])",
        )
        .bind(&worker.registry_id)
        .bind(&worker.organization_id)
        .bind(&worker.source_record_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|_| MatchingStoreError::Unavailable)?;
        let code = match current {
            None => ConsumerReceiptCode::SourceScopeMismatch,
            Some(row) => {
                let registry: String = row
                    .try_get("registry_id")
                    .map_err(|_| MatchingStoreError::Unavailable)?;
                let organization: String = row
                    .try_get("organization_id")
                    .map_err(|_| MatchingStoreError::Unavailable)?;
                let version: i64 = row
                    .try_get("current_receive_version")
                    .map_err(|_| MatchingStoreError::Unavailable)?;
                let state: String = row
                    .try_get("version_state")
                    .map_err(|_| MatchingStoreError::Unavailable)?;
                let source_active: bool = row
                    .try_get("source_active")
                    .map_err(|_| MatchingStoreError::Unavailable)?;
                let version_active: bool = row
                    .try_get("version_active")
                    .map_err(|_| MatchingStoreError::Unavailable)?;
                if registry != worker.registry_id || organization != worker.organization_id {
                    ConsumerReceiptCode::SourceScopeMismatch
                } else if !source_active || !version_active {
                    ConsumerReceiptCode::SourceWithdrawn
                } else if version != worker.expected_receive_version {
                    ConsumerReceiptCode::SourceSuperseded
                } else if state != "accepted" {
                    ConsumerReceiptCode::SourceMalformed
                } else {
                    code
                }
            }
        };
        insert_obsolete_receipt(&mut tx, worker, code).await?;
        tx.commit()
            .await
            .map_err(|_| MatchingStoreError::Unavailable)?;
        Ok(PersistedMatch::Obsolete { code })
    }

    async fn assert_worker_lease(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        worker: &ConsumerLease,
    ) -> Result<(), MatchingStoreError> {
        let row = sqlx::query(
            "SELECT registry_id,organization_id,source_record_id::text AS source_record_id,expected_receive_version,lease_token::text AS lease_token \
             FROM litecr.consumer_job WHERE consumer_name='advisory-matcher-v1' AND event_id=$1::uuid \
             AND state='leased' AND lease_owner=$2 AND lease_token=$3::uuid AND lease_expires_at > now() FOR UPDATE",
        )
        .bind(&worker.event_id)
        .bind(&worker.lease_owner)
        .bind(&worker.lease_token)
        .fetch_optional(&mut **tx)
        .await
        .map_err(|_| MatchingStoreError::Unavailable)?
        .ok_or(MatchingStoreError::Unavailable)?;
        let registry: String = row
            .try_get("registry_id")
            .map_err(|_| MatchingStoreError::Unavailable)?;
        let organization: String = row
            .try_get("organization_id")
            .map_err(|_| MatchingStoreError::Unavailable)?;
        let source: String = row
            .try_get("source_record_id")
            .map_err(|_| MatchingStoreError::Unavailable)?;
        let version: i64 = row
            .try_get("expected_receive_version")
            .map_err(|_| MatchingStoreError::Unavailable)?;
        let token: String = row
            .try_get("lease_token")
            .map_err(|_| MatchingStoreError::Unavailable)?;
        if registry != worker.registry_id
            || organization != worker.organization_id
            || source != worker.source_record_id
            || version != worker.expected_receive_version
            || token != worker.lease_token
        {
            return Err(MatchingStoreError::Unavailable);
        }
        Ok(())
    }
}

/// Materialize the exact keys used by the pure matcher in the same transaction
/// that accepts a source version. Conflict versions deliberately remain unindexed.
pub(crate) async fn index_accepted_source_version(
    tx: &mut Transaction<'_, Postgres>,
    source_record_id: &str,
    registry_id: &str,
    organization_id: &str,
    receive_version: i64,
    assertions: &[SourceAssertion],
    identifiers: &[IdentifierAssertion],
) -> Result<(), sqlx::Error> {
    for assertion in assertions {
        let is_original_name = is_matchable_original_name_field_path(&assertion.field_path);
        let is_birth_date = is_birth_date_field(&assertion.field_path);
        let key = normalized_assertion_key(&assertion.original_value);
        if (is_original_name || is_birth_date) && !key.is_empty() {
            sqlx::query("INSERT INTO matching.normalized_assertion(source_record_id,registry_id,organization_id,receive_version,field_path,normalizer_version,normalized_key,representation) VALUES($1::uuid,$2,$3,$4,$5,$6,$7,'source_original') ON CONFLICT DO NOTHING")
                .bind(source_record_id).bind(registry_id).bind(organization_id).bind(receive_version)
                .bind(&assertion.field_path).bind(NORMALIZATION_VERSION).bind(&key)
                .execute(&mut **tx).await?;
        }
        if is_birth_date {
            if let Some(year) = birth_year(&assertion.original_value) {
                sqlx::query("INSERT INTO matching.normalized_assertion(source_record_id,registry_id,organization_id,receive_version,field_path,normalizer_version,normalized_key,representation) VALUES($1::uuid,$2,$3,$4,$5,$6,$7,'source_variant') ON CONFLICT DO NOTHING")
                    .bind(source_record_id).bind(registry_id).bind(organization_id).bind(receive_version)
                    .bind(&assertion.field_path).bind(NORMALIZATION_VERSION).bind(year)
                    .execute(&mut **tx).await?;
            }
        }
    }
    for identifier in identifiers
        .iter()
        .filter(|identifier| identifier.authoritative)
    {
        if let Some(key) = authoritative_identifier_key(&identifier.value) {
            sqlx::query("INSERT INTO matching.normalized_identifier(source_record_id,registry_id,organization_id,receive_version,authority,identifier_type,normalizer_version,normalized_key) VALUES($1::uuid,$2,$3,$4,$5,$6,$7,$8) ON CONFLICT DO NOTHING")
                .bind(source_record_id).bind(registry_id).bind(organization_id).bind(receive_version)
                .bind(&identifier.authority).bind(&identifier.identifier_type).bind(NORMALIZATION_VERSION).bind(&key)
                .execute(&mut **tx).await?;
        }
    }
    sqlx::query("INSERT INTO matching.indexed_source_version(source_record_id,receive_version,normalizer_version) VALUES($1::uuid,$2,$3) ON CONFLICT DO NOTHING")
        .bind(source_record_id).bind(receive_version).bind(NORMALIZATION_VERSION)
        .execute(&mut **tx).await?;
    Ok(())
}

fn is_birth_date_field(field_path: &str) -> bool {
    field_path.eq_ignore_ascii_case("birth_date")
        || field_path.eq_ignore_ascii_case("date_of_birth")
}

fn birth_year(value: &str) -> Option<&str> {
    let year = value.get(0..4)?;
    (year.bytes().all(|byte| byte.is_ascii_digit())).then_some(year)
}

#[async_trait]
impl MatchingStore for PostgresMatchingStore {
    async fn evaluate(
        &self,
        registry_id: &str,
        organization_id: &str,
        query_source_record_id: &str,
        candidate_limit: usize,
        actor_id: &str,
        request_id: &str,
        idempotency: Option<MatchIdempotency>,
    ) -> Result<MatchResult, MatchingStoreError> {
        match self
            .evaluate_internal(MatchEvaluationRequest {
                registry_id,
                organization_id,
                query_source_record_id,
                candidate_limit,
                actor_id,
                request_id,
                idempotency,
                worker: None,
            })
            .await?
        {
            PersistedMatch::Evidence {
                result: Some(result),
                evidence_run_id,
            } => {
                let mut result = *result;
                result.evidence_run_id = Some(evidence_run_id);
                Ok(result)
            }
            _ => Err(MatchingStoreError::Unavailable),
        }
    }

    async fn read_evidence(
        &self,
        evidence_run_id: &str,
        registry_id: &str,
        organization_id: &str,
    ) -> Result<MatchResult, MatchingStoreError> {
        let Some(run) = sqlx::query(
            "SELECT run_id::text AS run_id,query_source_record_id::text AS query_source_record_id,query_receive_version,query_source_version,engine_version,normalization_version,candidate_version,feature_version,policy_version,candidate_count,candidates_truncated,result_snapshot \
             FROM matching.evidence_run WHERE run_id=$1::uuid AND registry_id=$2 AND organization_id=$3",
        )
        .bind(evidence_run_id)
        .bind(registry_id)
        .bind(organization_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| MatchingStoreError::Unavailable)?
        else {
            return Err(MatchingStoreError::EvidenceNotFound);
        };

        let run_id: String = run
            .try_get("run_id")
            .map_err(|_| MatchingStoreError::Unavailable)?;
        let query_source_record_id: String = run
            .try_get("query_source_record_id")
            .map_err(|_| MatchingStoreError::Unavailable)?;
        let query_receive_version: i64 = run
            .try_get("query_receive_version")
            .map_err(|_| MatchingStoreError::Unavailable)?;
        let query_source_version: Option<String> = run
            .try_get("query_source_version")
            .map_err(|_| MatchingStoreError::Unavailable)?;
        let engine_version: String = run
            .try_get("engine_version")
            .map_err(|_| MatchingStoreError::Unavailable)?;
        let normalization_version: String = run
            .try_get("normalization_version")
            .map_err(|_| MatchingStoreError::Unavailable)?;
        let candidate_version: String = run
            .try_get("candidate_version")
            .map_err(|_| MatchingStoreError::Unavailable)?;
        let feature_version: String = run
            .try_get("feature_version")
            .map_err(|_| MatchingStoreError::Unavailable)?;
        let policy_version: String = run
            .try_get("policy_version")
            .map_err(|_| MatchingStoreError::Unavailable)?;
        let candidate_count: i32 = run
            .try_get("candidate_count")
            .map_err(|_| MatchingStoreError::Unavailable)?;
        let truncated: bool = run
            .try_get("candidates_truncated")
            .map_err(|_| MatchingStoreError::Unavailable)?;
        let snapshot: Option<Value> = run
            .try_get("result_snapshot")
            .map_err(|_| MatchingStoreError::Unavailable)?;
        if let Some(snapshot) = snapshot {
            let result: MatchResult =
                serde_json::from_value(snapshot).map_err(|_| MatchingStoreError::Unavailable)?;
            if result.evidence_run_id.as_deref() != Some(run_id.as_str())
                || !result.snapshot_complete
                || !result.retrieval.complete
                || result.query_record_id != query_source_record_id
                || result.query_receive_version != Some(query_receive_version)
                || result.query_source_version != query_source_version
                || result.versions.engine != engine_version
                || result.versions.normalization != normalization_version
                || result.versions.candidate != candidate_version
                || result.versions.feature != feature_version
                || result.versions.policy != policy_version
                || result.candidates.len() != candidate_count as usize
                || result.retrieval.truncated != truncated
            {
                return Err(MatchingStoreError::Unavailable);
            }
            return Ok(result);
        }

        // Pre-schema-17 runs did not retain the full response. Reconstruct the
        // candidate list from immutable evidence rows and explicitly mark the
        // unavailable telemetry/order information as incomplete.
        let rows = sqlx::query(
            "SELECT evidence FROM matching.evidence_candidate WHERE run_id=$1::uuid ORDER BY score_points DESC,candidate_source_record_id",
        )
        .bind(&run_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|_| MatchingStoreError::Unavailable)?;
        let candidates = rows
            .into_iter()
            .map(|row| {
                let evidence: Value = row
                    .try_get("evidence")
                    .map_err(|_| MatchingStoreError::Unavailable)?;
                serde_json::from_value(evidence).map_err(|_| MatchingStoreError::Unavailable)
            })
            .collect::<Result<Vec<litecr_matching::CandidateEvidence>, _>>()?;
        if candidate_count < 0 || candidate_count as usize != candidates.len() {
            return Err(MatchingStoreError::Unavailable);
        }
        let mut per_pass_candidates = BTreeMap::new();
        for candidate in &candidates {
            for pass in &candidate.passes {
                *per_pass_candidates.entry(*pass).or_insert(0) += 1;
            }
        }
        Ok(MatchResult {
            versions: litecr_matching::MatcherVersions {
                engine: engine_version,
                normalization: normalization_version,
                candidate: candidate_version,
                feature: feature_version,
                policy: policy_version,
            },
            evidence_run_id: Some(run_id),
            query_record_id: query_source_record_id,
            query_source_version,
            query_receive_version: Some(query_receive_version),
            retrieval: litecr_matching::RetrievalTelemetry {
                per_pass_candidates,
                // This is the retained candidate count (a lower bound when
                // truncated), never an invented pre-limit count.
                union_before_limit: candidate_count as usize,
                returned_candidates: candidates.len(),
                truncated,
                limit: candidate_count as usize,
                complete: false,
            },
            candidates,
            mutation_authority: "registry-only; advisory output cannot link or merge".into(),
            snapshot_complete: false,
        })
    }
}

enum PersistedMatch {
    Evidence {
        result: Option<Box<MatchResult>>,
        evidence_run_id: String,
    },
    Obsolete {
        code: ConsumerReceiptCode,
    },
}

async fn insert_obsolete_receipt(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    worker: &ConsumerLease,
    code: ConsumerReceiptCode,
) -> Result<(), MatchingStoreError> {
    sqlx::query("INSERT INTO litecr.consumer_receipt(consumer_name,event_id,outcome,receipt_code) VALUES('advisory-matcher-v1',$1::uuid,'obsolete',$2) ON CONFLICT (consumer_name,event_id) DO NOTHING")
        .bind(&worker.event_id).bind(code.as_str()).execute(&mut **tx).await
        .map_err(|_| MatchingStoreError::Unavailable)?;
    sqlx::query("UPDATE litecr.consumer_job SET state='obsolete',lease_owner=NULL,lease_token=NULL,lease_expires_at=NULL,updated_at=now(),last_error_code=NULL WHERE consumer_name='advisory-matcher-v1' AND event_id=$1::uuid AND state='leased' AND lease_owner=$2 AND lease_token=$3::uuid")
        .bind(&worker.event_id).bind(&worker.lease_owner).bind(&worker.lease_token).execute(&mut **tx).await
        .map_err(|_| MatchingStoreError::Unavailable)?;
    Ok(())
}

fn receipt_code_from_str(value: &str) -> ConsumerReceiptCode {
    match value {
        "SOURCE_WITHDRAWN" => ConsumerReceiptCode::SourceWithdrawn,
        "SOURCE_SCOPE_MISMATCH" => ConsumerReceiptCode::SourceScopeMismatch,
        "SOURCE_MALFORMED" => ConsumerReceiptCode::SourceMalformed,
        "RETRY_EXHAUSTED" => ConsumerReceiptCode::RetryExhausted,
        _ => ConsumerReceiptCode::SourceSuperseded,
    }
}

fn date_precision(value: &str) -> String {
    match value.len() {
        4 => "year",
        7 => "month",
        10 => "day",
        _ => "unknown",
    }
    .into()
}

#[cfg(test)]
mod admission_tests {
    use super::*;

    async fn store() -> (PostgresMatchingStore, tokio::net::TcpListener) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(10)
            .connect_lazy(&format!(
                "postgres://synthetic@{}/synthetic_unused",
                listener.local_addr().unwrap()
            ))
            .unwrap();
        (PostgresMatchingStore::new(pool), listener)
    }

    fn request() -> MatchEvaluationRequest<'static> {
        MatchEvaluationRequest {
            registry_id: "synthetic",
            organization_id: "synthetic",
            query_source_record_id: "00000000-0000-0000-0000-000000000001",
            candidate_limit: 10,
            actor_id: "synthetic",
            request_id: "synthetic",
            idempotency: None,
            worker: None,
        }
    }

    #[tokio::test]
    async fn saturated_evaluations_reject_before_database_access_and_clones_share_capacity() {
        let (store, listener) = store().await;
        let clone = store.clone();
        let held = store
            .evaluation_admission
            .clone()
            .acquire_many_owned(128)
            .await
            .unwrap();
        assert!(matches!(
            clone.evaluate_internal(request()).await,
            Err(MatchingStoreError::Unavailable)
        ));
        assert_eq!(store.pool.size(), 0);
        assert!(
            tokio::time::timeout(Duration::from_millis(10), listener.accept())
                .await
                .is_err(),
            "queued or rejected evaluations must not even open a database socket"
        );
        drop(held);
        assert_eq!(clone.evaluation_admission.available_permits(), 128);
    }

    #[tokio::test]
    async fn configurable_parallelism_bounds_and_shared_cancellation() {
        for active in [2, 4, 8] {
            let (original, listener) = store().await;
            let store =
                PostgresMatchingStore::with_evaluation_parallelism(original.pool.clone(), active)
                    .unwrap();
            let clone = store.clone();
            let held = store
                .evaluation_active
                .clone()
                .acquire_many_owned(active as u32)
                .await
                .unwrap();
            assert!(
                tokio::time::timeout(
                    Duration::from_millis(10),
                    clone.evaluate_internal(request())
                )
                .await
                .is_err()
            );
            assert_eq!(store.pool.size(), 0);
            assert!(
                tokio::time::timeout(Duration::from_millis(10), listener.accept())
                    .await
                    .is_err()
            );
            assert_eq!(store.evaluation_admission.available_permits(), 128);
            drop(held);
            assert_eq!(clone.evaluation_active.available_permits(), active);
            assert!(
                PostgresMatchingStore::with_evaluation_parallelism(original.pool.clone(), 0)
                    .is_err()
            );
            assert!(
                PostgresMatchingStore::with_evaluation_parallelism(original.pool.clone(), 9)
                    .is_err()
            );
        }
    }

    #[tokio::test]
    async fn cancelled_queued_evaluation_releases_admission_without_holding_a_connection() {
        let (store, listener) = store().await;
        let held = store
            .evaluation_active
            .clone()
            .acquire_many_owned(2)
            .await
            .unwrap();
        assert!(
            tokio::time::timeout(
                Duration::from_millis(10),
                store.evaluate_internal(request())
            )
            .await
            .is_err()
        );
        assert_eq!(store.pool.size(), 0);
        assert!(
            tokio::time::timeout(Duration::from_millis(10), listener.accept())
                .await
                .is_err(),
            "queued or rejected evaluations must not even open a database socket"
        );
        assert_eq!(store.evaluation_admission.available_permits(), 128);
        drop(held);
        assert_eq!(store.evaluation_active.available_permits(), 2);
    }
}
