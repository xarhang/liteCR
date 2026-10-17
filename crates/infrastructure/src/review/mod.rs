//! PostgreSQL review adapter. Identity effects share this transaction with decisions/audit/outbox.
use async_trait::async_trait;
use litecr_application::review::{
    ReviewActor, ReviewCaseCursor, ReviewCaseDetail, ReviewCasePage, ReviewCaseView,
    ReviewDetailAccess, ReviewQueueFilter, ReviewResult, ReviewStore, ReviewStoreError,
};
use litecr_domain::review::{
    OpenReviewCase, ReviewDecisionCommand, ReviewDecisionHistory, ReviewDecisionKind,
};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Postgres, Row, Transaction};

#[derive(Clone)]
pub struct PostgresReviewStore {
    pool: PgPool,
}

struct OutboxEvent<'a> {
    registry: &'a str,
    organization: &'a str,
    aggregate: &'a str,
    aggregate_id: &'a str,
    version: i64,
    kind: &'a str,
    payload: serde_json::Value,
}

impl PostgresReviewStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

fn command_fingerprint(c: &ReviewDecisionCommand) -> [u8; 32] {
    let mut h = Sha256::new();
    for s in [
        c.case_id.as_str(),
        c.reason_code.as_str(),
        c.reason.as_str(),
        c.merge_id.as_deref().unwrap_or(""),
    ] {
        h.update((s.len() as u64).to_be_bytes());
        h.update(s.as_bytes());
    }
    h.update(c.expected_case_version.to_be_bytes());
    h.update(c.expected_query_receive_version.to_be_bytes());
    h.update(c.expected_candidate_receive_version.to_be_bytes());
    h.update(c.expected_query_person_version.to_be_bytes());
    h.update(c.expected_candidate_person_version.to_be_bytes());
    h.update([match c.kind {
        ReviewDecisionKind::ConfirmedNonMatch => 1,
        ReviewDecisionKind::Link => 2,
        ReviewDecisionKind::Merge => 3,
        ReviewDecisionKind::ReverseMerge => 4,
    }]);
    h.finalize().into()
}
fn key_hash(key: &str) -> [u8; 32] {
    Sha256::digest(key.as_bytes()).into()
}
fn decision_name(d: &ReviewDecisionKind) -> &'static str {
    match d {
        ReviewDecisionKind::ConfirmedNonMatch => "confirmed_non_match",
        ReviewDecisionKind::Link => "link",
        ReviewDecisionKind::Merge => "merge",
        ReviewDecisionKind::ReverseMerge => "reverse_merge",
    }
}

async fn case_view(
    tx: &mut Transaction<'_, Postgres>,
    case_id: &str,
    registry: &str,
    org: &str,
    access: Option<&ReviewDetailAccess>,
) -> Result<ReviewCaseView, ReviewStoreError> {
    let row = sqlx::query(
        "SELECT c.case_id::text, c.registry_id, c.organization_id, c.query_source_record_id::text, c.candidate_source_record_id::text, c.evidence_run_id::text AS evidence_run_id, c.query_receive_version, c.candidate_receive_version, c.case_version, c.state, c.assigned_actor_id, c.assigned_actor_issuer_url, p1.resource_version AS query_person_version, p2.resource_version AS candidate_person_version FROM litecr.review_case c JOIN litecr.source_record s1 ON s1.source_record_id=c.query_source_record_id JOIN litecr.source_record s2 ON s2.source_record_id=c.candidate_source_record_id JOIN litecr.person p1 ON p1.person_id=s1.person_id JOIN litecr.person p2 ON p2.person_id=s2.person_id WHERE c.case_id=$1::uuid AND c.registry_id=$2 AND c.organization_id=$3 AND s1.withdrawn_at IS NULL AND s2.withdrawn_at IS NULL AND NOT EXISTS (SELECT 1 FROM litecr.source_record_version v WHERE v.source_record_id=s1.source_record_id AND v.receive_version=c.query_receive_version AND v.withdrawn_at IS NOT NULL) AND NOT EXISTS (SELECT 1 FROM litecr.source_record_version v WHERE v.source_record_id=s2.source_record_id AND v.receive_version=c.candidate_receive_version AND v.withdrawn_at IS NOT NULL) AND ($4::text IS NULL OR ($8::boolean AND $7::text IS NOT NULL AND c.assigned_actor_id=$4 AND c.assigned_actor_issuer_url=$7) OR $5::boolean OR (c.query_source_record_id::text=ANY($6::text[]) AND c.candidate_source_record_id::text=ANY($6::text[]))) FOR SHARE OF c",
    )
    .bind(case_id)
    .bind(registry)
    .bind(org)
    .bind(access.map(|value| value.actor_id.as_str()))
    .bind(access.is_some_and(|value| value.all_records))
    .bind(access.map_or_else(Vec::new, |value| value.record_ids.clone()))
    .bind(access.and_then(|value| value.actor_issuer_url.as_deref()))
    .bind(access.is_some_and(|value| value.assigned_case_allowed))
    .fetch_optional(&mut **tx)
    .await
    .map_err(|_| ReviewStoreError::Unavailable)?
    .ok_or(ReviewStoreError::NotFound)?;
    Ok(ReviewCaseView {
        case_id: row
            .try_get("case_id")
            .map_err(|_| ReviewStoreError::Unavailable)?,
        registry_id: row
            .try_get("registry_id")
            .map_err(|_| ReviewStoreError::Unavailable)?,
        organization_id: row
            .try_get("organization_id")
            .map_err(|_| ReviewStoreError::Unavailable)?,
        query_source_record_id: row
            .try_get("query_source_record_id")
            .map_err(|_| ReviewStoreError::Unavailable)?,
        candidate_source_record_id: row
            .try_get("candidate_source_record_id")
            .map_err(|_| ReviewStoreError::Unavailable)?,
        evidence_run_id: row
            .try_get("evidence_run_id")
            .map_err(|_| ReviewStoreError::Unavailable)?,
        query_receive_version: row
            .try_get("query_receive_version")
            .map_err(|_| ReviewStoreError::Unavailable)?,
        candidate_receive_version: row
            .try_get("candidate_receive_version")
            .map_err(|_| ReviewStoreError::Unavailable)?,
        case_version: row
            .try_get("case_version")
            .map_err(|_| ReviewStoreError::Unavailable)?,
        state: row
            .try_get("state")
            .map_err(|_| ReviewStoreError::Unavailable)?,
        assigned_actor_id: row
            .try_get("assigned_actor_id")
            .map_err(|_| ReviewStoreError::Unavailable)?,
        assigned_actor_issuer_url: row
            .try_get("assigned_actor_issuer_url")
            .map_err(|_| ReviewStoreError::Unavailable)?,
        query_person_version: row
            .try_get("query_person_version")
            .map_err(|_| ReviewStoreError::Unavailable)?,
        candidate_person_version: row
            .try_get("candidate_person_version")
            .map_err(|_| ReviewStoreError::Unavailable)?,
    })
}

async fn audit(
    tx: &mut Transaction<'_, Postgres>,
    actor: &ReviewActor,
    registry: &str,
    org: &str,
    action: &str,
    resource: &str,
    version: i64,
) -> Result<(), ReviewStoreError> {
    sqlx::query("INSERT INTO litecr.mutation_audit (actor_kind,actor_issuer_url,actor_id,registry_id,organization_id,action,resource_type,resource_ref,outcome,reason_code,request_id,correlation_id,resource_version) VALUES ('human',$1,$2,$3,$4,$5,'review_case',$6,'accepted',$7,$8,$9,$10)")
        .bind(actor.issuer_url.as_deref().unwrap_or("")).bind(&actor.id).bind(registry).bind(org).bind(action).bind(resource).bind(action).bind(&actor.request_id).bind(&actor.correlation_id).bind(version)
        .execute(&mut **tx).await.map_err(|_|ReviewStoreError::Unavailable)?;
    Ok(())
}
async fn event(
    tx: &mut Transaction<'_, Postgres>,
    actor: &ReviewActor,
    event: OutboxEvent<'_>,
) -> Result<(), ReviewStoreError> {
    sqlx::query("INSERT INTO litecr.outbox_event (event_type,schema_version,registry_id,organization_id,aggregate_type,aggregate_id,aggregate_version,actor_kind,actor_id,correlation_id,payload) VALUES ($1,1,$2,$3,$4,$5::uuid,$6,'human',$7,$8,$9)")
        .bind(event.kind).bind(event.registry).bind(event.organization).bind(event.aggregate).bind(event.aggregate_id).bind(event.version).bind(&actor.id).bind(&actor.correlation_id).bind(event.payload)
        .execute(&mut **tx).await.map_err(|_|ReviewStoreError::Unavailable)?;
    Ok(())
}

#[async_trait]
impl ReviewStore for PostgresReviewStore {
    async fn detail(
        &self,
        case_id: &str,
        registry: &str,
        org: &str,
        access: &ReviewDetailAccess,
    ) -> Result<ReviewCaseDetail, ReviewStoreError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|_| ReviewStoreError::Unavailable)?;
        let case = case_view(&mut tx, case_id, registry, org, Some(access)).await?;
        let rows=sqlx::query("SELECT d.decision_id::text,d.decision,d.outcome,d.reason_code,d.reason,d.actor_id,d.actor_issuer_url,to_char(d.created_at AT TIME ZONE 'UTC','YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') AS occurred_at,d.merge_id::text FROM litecr.review_decision d JOIN litecr.review_case c ON c.case_id=d.case_id WHERE d.case_id=$1::uuid AND c.registry_id=$2 AND c.organization_id=$3 ORDER BY d.created_at,d.decision_id")
            .bind(case_id).bind(registry).bind(org).fetch_all(&mut *tx).await.map_err(|_|ReviewStoreError::Unavailable)?;
        let history = rows
            .into_iter()
            .map(|row| {
                Ok(ReviewDecisionHistory {
                    decision_id: row
                        .try_get("decision_id")
                        .map_err(|_| ReviewStoreError::Unavailable)?,
                    decision: row
                        .try_get("decision")
                        .map_err(|_| ReviewStoreError::Unavailable)?,
                    outcome: row
                        .try_get("outcome")
                        .map_err(|_| ReviewStoreError::Unavailable)?,
                    reason_code: row
                        .try_get("reason_code")
                        .map_err(|_| ReviewStoreError::Unavailable)?,
                    reason: row
                        .try_get("reason")
                        .map_err(|_| ReviewStoreError::Unavailable)?,
                    actor_id: row
                        .try_get("actor_id")
                        .map_err(|_| ReviewStoreError::Unavailable)?,
                    actor_issuer_url: row
                        .try_get("actor_issuer_url")
                        .map_err(|_| ReviewStoreError::Unavailable)?,
                    occurred_at: row
                        .try_get("occurred_at")
                        .map_err(|_| ReviewStoreError::Unavailable)?,
                    merge_id: row
                        .try_get("merge_id")
                        .map_err(|_| ReviewStoreError::Unavailable)?,
                })
            })
            .collect::<Result<Vec<_>, ReviewStoreError>>()?;
        tx.commit()
            .await
            .map_err(|_| ReviewStoreError::Unavailable)?;
        Ok(ReviewCaseDetail { case, history })
    }

    async fn list_cases(
        &self,
        registry: &str,
        org: &str,
        queue_filter: ReviewQueueFilter,
        limit: u16,
        after: Option<&ReviewCaseCursor>,
    ) -> Result<ReviewCasePage, ReviewStoreError> {
        let rows=sqlx::query("SELECT c.case_id::text,c.registry_id,c.organization_id,c.query_source_record_id::text,c.candidate_source_record_id::text,c.evidence_run_id::text AS evidence_run_id,c.query_receive_version,c.candidate_receive_version,c.case_version,c.state,c.assigned_actor_id,c.assigned_actor_issuer_url,p1.resource_version AS query_person_version,p2.resource_version AS candidate_person_version,to_char(c.created_at AT TIME ZONE 'UTC','YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') AS cursor_created_at FROM litecr.review_case c JOIN litecr.source_record s1 ON s1.source_record_id=c.query_source_record_id JOIN litecr.source_record s2 ON s2.source_record_id=c.candidate_source_record_id JOIN litecr.person p1 ON p1.person_id=s1.person_id JOIN litecr.person p2 ON p2.person_id=s2.person_id WHERE c.registry_id=$1 AND c.organization_id=$2 AND ($3='all' OR ($3='active' AND c.state IN ('open','assigned')) OR ($3='resolved' AND c.state IN ('decided','closed'))) AND s1.withdrawn_at IS NULL AND s2.withdrawn_at IS NULL AND NOT EXISTS (SELECT 1 FROM litecr.source_record_version v WHERE v.source_record_id=s1.source_record_id AND v.receive_version=c.query_receive_version AND v.withdrawn_at IS NOT NULL) AND NOT EXISTS (SELECT 1 FROM litecr.source_record_version v WHERE v.source_record_id=s2.source_record_id AND v.receive_version=c.candidate_receive_version AND v.withdrawn_at IS NOT NULL) AND ($4::timestamptz IS NULL OR (c.created_at,c.case_id)<($4::timestamptz,$5::uuid)) ORDER BY c.created_at DESC,c.case_id DESC LIMIT $6")
            .bind(registry).bind(org).bind(queue_filter.as_str()).bind(after.map(|cursor| cursor.created_at.as_str())).bind(after.map(|cursor| cursor.case_id.as_str())).bind(i64::from(limit) + 1).fetch_all(&self.pool).await.map_err(|_|ReviewStoreError::Unavailable)?;
        let has_more = rows.len() > usize::from(limit);
        let rows = rows.into_iter().take(usize::from(limit));
        let mut cases = Vec::new();
        let mut next_cursor = None;
        for row in rows {
            let case_id: String = row
                .try_get("case_id")
                .map_err(|_| ReviewStoreError::Unavailable)?;
            let created_at: String = row
                .try_get("cursor_created_at")
                .map_err(|_| ReviewStoreError::Unavailable)?;
            cases.push(ReviewCaseView {
                case_id: case_id.clone(),
                registry_id: row
                    .try_get("registry_id")
                    .map_err(|_| ReviewStoreError::Unavailable)?,
                organization_id: row
                    .try_get("organization_id")
                    .map_err(|_| ReviewStoreError::Unavailable)?,
                query_source_record_id: row
                    .try_get("query_source_record_id")
                    .map_err(|_| ReviewStoreError::Unavailable)?,
                candidate_source_record_id: row
                    .try_get("candidate_source_record_id")
                    .map_err(|_| ReviewStoreError::Unavailable)?,
                evidence_run_id: row
                    .try_get("evidence_run_id")
                    .map_err(|_| ReviewStoreError::Unavailable)?,
                query_receive_version: row
                    .try_get("query_receive_version")
                    .map_err(|_| ReviewStoreError::Unavailable)?,
                candidate_receive_version: row
                    .try_get("candidate_receive_version")
                    .map_err(|_| ReviewStoreError::Unavailable)?,
                query_person_version: row
                    .try_get("query_person_version")
                    .map_err(|_| ReviewStoreError::Unavailable)?,
                candidate_person_version: row
                    .try_get("candidate_person_version")
                    .map_err(|_| ReviewStoreError::Unavailable)?,
                case_version: row
                    .try_get("case_version")
                    .map_err(|_| ReviewStoreError::Unavailable)?,
                state: row
                    .try_get("state")
                    .map_err(|_| ReviewStoreError::Unavailable)?,
                assigned_actor_id: row
                    .try_get("assigned_actor_id")
                    .map_err(|_| ReviewStoreError::Unavailable)?,
                assigned_actor_issuer_url: row
                    .try_get("assigned_actor_issuer_url")
                    .map_err(|_| ReviewStoreError::Unavailable)?,
            });
            next_cursor = Some(ReviewCaseCursor {
                queue_filter,
                created_at,
                case_id,
            });
        }
        Ok(ReviewCasePage {
            cases,
            has_more,
            next_cursor: has_more.then_some(next_cursor).flatten(),
        })
    }

    async fn open_case(
        &self,
        actor: &ReviewActor,
        c: &OpenReviewCase,
    ) -> Result<ReviewCaseView, ReviewStoreError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|_| ReviewStoreError::Unavailable)?;
        let row=sqlx::query("SELECT s1.current_receive_version AS qv,s2.current_receive_version AS cv FROM litecr.source_record s1 JOIN litecr.source_record s2 ON s2.registry_id=s1.registry_id AND s2.organization_id=s1.organization_id JOIN litecr.source_record_version v1 ON v1.source_record_id=s1.source_record_id AND v1.receive_version=s1.current_receive_version JOIN litecr.source_record_version v2 ON v2.source_record_id=s2.source_record_id AND v2.receive_version=s2.current_receive_version WHERE s1.source_record_id=$1::uuid AND s2.source_record_id=$2::uuid AND s1.registry_id=$3 AND s1.organization_id=$4 AND s1.withdrawn_at IS NULL AND s2.withdrawn_at IS NULL AND v1.withdrawn_at IS NULL AND v2.withdrawn_at IS NULL FOR UPDATE OF s1,s2")
            .bind(&c.query_source_record_id).bind(&c.candidate_source_record_id).bind(&c.registry_id).bind(&c.organization_id).fetch_optional(&mut *tx).await.map_err(|_|ReviewStoreError::Unavailable)?.ok_or(ReviewStoreError::NotFound)?;
        let qv: i64 = row
            .try_get("qv")
            .map_err(|_| ReviewStoreError::Unavailable)?;
        let cv: i64 = row
            .try_get("cv")
            .map_err(|_| ReviewStoreError::Unavailable)?;
        if qv != c.expected_query_receive_version || cv != c.expected_candidate_receive_version {
            return Err(ReviewStoreError::Conflict);
        }
        if let Some(run_id) = c.evidence_run_id.as_deref() {
            let matches: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM matching.evidence_run r JOIN matching.evidence_candidate ec ON ec.run_id=r.run_id WHERE r.run_id=$1::uuid AND r.registry_id=$2 AND r.organization_id=$3 AND r.query_source_record_id=$4::uuid AND r.query_receive_version=$5 AND ec.candidate_source_record_id=$6::uuid AND ec.candidate_receive_version=$7)")
                .bind(run_id).bind(&c.registry_id).bind(&c.organization_id).bind(&c.query_source_record_id).bind(qv).bind(&c.candidate_source_record_id).bind(cv)
                .fetch_one(&mut *tx).await.map_err(|_| ReviewStoreError::Unavailable)?;
            if !matches {
                return Err(ReviewStoreError::EvidenceMismatch);
            }
        }
        let inserted=sqlx::query("INSERT INTO litecr.review_case (registry_id,organization_id,query_source_record_id,candidate_source_record_id,evidence_run_id,query_receive_version,candidate_receive_version,created_by) VALUES ($1,$2,$3::uuid,$4::uuid,$5::uuid,$6,$7,$8) ON CONFLICT (registry_id,organization_id,query_source_record_id,candidate_source_record_id,query_receive_version,candidate_receive_version) DO NOTHING RETURNING case_id::text")
            .bind(&c.registry_id).bind(&c.organization_id).bind(&c.query_source_record_id).bind(&c.candidate_source_record_id).bind(&c.evidence_run_id).bind(qv).bind(cv).bind(&actor.id).fetch_optional(&mut *tx).await.map_err(|_|ReviewStoreError::Unavailable)?;
        let created = inserted.is_some();
        let id: String = if let Some(row) = inserted {
            row.try_get("case_id")
                .map_err(|_| ReviewStoreError::Unavailable)?
        } else {
            sqlx::query_scalar("SELECT case_id::text FROM litecr.review_case WHERE registry_id=$1 AND organization_id=$2 AND query_source_record_id=$3::uuid AND candidate_source_record_id=$4::uuid AND query_receive_version=$5 AND candidate_receive_version=$6")
                .bind(&c.registry_id).bind(&c.organization_id).bind(&c.query_source_record_id).bind(&c.candidate_source_record_id).bind(qv).bind(cv).fetch_one(&mut *tx).await.map_err(|_|ReviewStoreError::Unavailable)?
        };
        let view = case_view(&mut tx, &id, &c.registry_id, &c.organization_id, None).await?;
        if created {
            audit(
                &mut tx,
                actor,
                &c.registry_id,
                &c.organization_id,
                "review.case.opened",
                &id,
                view.case_version,
            )
            .await?;
            event(
                &mut tx,
                actor,
                OutboxEvent {
                    registry: &c.registry_id,
                    organization: &c.organization_id,
                    aggregate: "review_case",
                    aggregate_id: &id,
                    version: view.case_version,
                    kind: "review.case.opened",
                    payload: serde_json::json!({"case_id":id,"query_source_record_id":c.query_source_record_id,"candidate_source_record_id":c.candidate_source_record_id,"evidence_run_id":c.evidence_run_id}),
                },
            ).await?;
        }
        tx.commit()
            .await
            .map_err(|_| ReviewStoreError::Unavailable)?;
        Ok(view)
    }

    async fn assign(
        &self,
        actor: &ReviewActor,
        case_id: &str,
        registry: &str,
        org: &str,
        expected: i64,
        assignee: &str,
    ) -> Result<ReviewCaseView, ReviewStoreError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|_| ReviewStoreError::Unavailable)?;
        let row=sqlx::query("UPDATE litecr.review_case c SET assigned_actor_id=$1,assigned_actor_issuer_url=$2,state='assigned',case_version=case_version+1,updated_at=now() WHERE case_id=$3::uuid AND registry_id=$4 AND organization_id=$5 AND case_version=$6 AND state IN ('open','assigned') AND EXISTS (SELECT 1 FROM litecr.source_record s1 JOIN litecr.source_record s2 ON s2.source_record_id=c.candidate_source_record_id WHERE s1.source_record_id=c.query_source_record_id AND s1.withdrawn_at IS NULL AND s2.withdrawn_at IS NULL) AND NOT EXISTS (SELECT 1 FROM litecr.source_record_version v WHERE v.source_record_id=c.query_source_record_id AND v.receive_version=c.query_receive_version AND v.withdrawn_at IS NOT NULL) AND NOT EXISTS (SELECT 1 FROM litecr.source_record_version v WHERE v.source_record_id=c.candidate_source_record_id AND v.receive_version=c.candidate_receive_version AND v.withdrawn_at IS NOT NULL) RETURNING case_version")
            .bind(assignee).bind(actor.issuer_url.as_deref()).bind(case_id).bind(registry).bind(org).bind(expected).fetch_optional(&mut *tx).await.map_err(|_|ReviewStoreError::Unavailable)?;
        let Some(row) = row else {
            return Err(ReviewStoreError::Conflict);
        };
        let version: i64 = row
            .try_get("case_version")
            .map_err(|_| ReviewStoreError::Unavailable)?;
        audit(
            &mut tx,
            actor,
            registry,
            org,
            "review.case.assigned",
            case_id,
            version,
        )
        .await?;
        event(
            &mut tx,
            actor,
            OutboxEvent {
                registry,
                organization: org,
                aggregate: "review_case",
                aggregate_id: case_id,
                version,
                kind: "review.case.updated",
                payload: serde_json::json!({"case_id":case_id,"state":"assigned"}),
            },
        )
        .await?;
        let view = case_view(&mut tx, case_id, registry, org, None).await?;
        tx.commit()
            .await
            .map_err(|_| ReviewStoreError::Unavailable)?;
        Ok(view)
    }

    async fn decide(
        &self,
        actor: &ReviewActor,
        registry: &str,
        org: &str,
        c: &ReviewDecisionCommand,
    ) -> Result<ReviewResult, ReviewStoreError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|_| ReviewStoreError::Unavailable)?;
        let key = key_hash(&c.idempotency_key);
        let fingerprint = command_fingerprint(c);
        if let Some(prior)=sqlx::query("SELECT command_fingerprint,decision_id::text,outcome,merge_id::text FROM litecr.review_decision WHERE case_id=$1::uuid AND actor_issuer_url=$2 AND actor_id=$3 AND idempotency_key_hash=$4 FOR UPDATE")
            .bind(&c.case_id).bind(actor.issuer_url.as_deref().unwrap_or("")).bind(&actor.id).bind(key.to_vec()).fetch_optional(&mut *tx).await.map_err(|_|ReviewStoreError::Unavailable)? {
            let old:Vec<u8>=prior.try_get("command_fingerprint").map_err(|_|ReviewStoreError::Unavailable)?;
            if old.as_slice()!=fingerprint { return Err(ReviewStoreError::Conflict); }
            let view=case_view(&mut tx,&c.case_id,registry,org,None).await?;
            let result=ReviewResult{case:view,decision_id:prior.try_get("decision_id").map_err(|_|ReviewStoreError::Unavailable)?,outcome:prior.try_get("outcome").map_err(|_|ReviewStoreError::Unavailable)?,merge_id:prior.try_get("merge_id").map_err(|_|ReviewStoreError::Unavailable)?,replayed:true};
            tx.commit().await.map_err(|_|ReviewStoreError::Unavailable)?; return Ok(result);
        }
        let locked=sqlx::query("SELECT c.case_id::text,c.registry_id,c.organization_id,c.query_source_record_id::text AS qid,c.candidate_source_record_id::text AS cid,c.case_version,c.state,s1.current_receive_version AS qrv,s2.current_receive_version AS crv,s1.person_id::text AS qpid,s2.person_id::text AS cpid FROM litecr.review_case c JOIN litecr.source_record s1 ON s1.source_record_id=c.query_source_record_id JOIN litecr.source_record s2 ON s2.source_record_id=c.candidate_source_record_id WHERE c.case_id=$1::uuid AND c.registry_id=$2 AND c.organization_id=$3 AND s1.withdrawn_at IS NULL AND s2.withdrawn_at IS NULL AND NOT EXISTS (SELECT 1 FROM litecr.source_record_version v WHERE v.source_record_id=s1.source_record_id AND v.receive_version=c.query_receive_version AND v.withdrawn_at IS NOT NULL) AND NOT EXISTS (SELECT 1 FROM litecr.source_record_version v WHERE v.source_record_id=s2.source_record_id AND v.receive_version=c.candidate_receive_version AND v.withdrawn_at IS NOT NULL) FOR UPDATE OF c,s1,s2")
            .bind(&c.case_id).bind(registry).bind(org).fetch_optional(&mut *tx).await.map_err(|_|ReviewStoreError::Unavailable)?.ok_or(ReviewStoreError::NotFound)?;
        let case_version: i64 = locked
            .try_get("case_version")
            .map_err(|_| ReviewStoreError::Unavailable)?;
        let qrv: i64 = locked
            .try_get("qrv")
            .map_err(|_| ReviewStoreError::Unavailable)?;
        let crv: i64 = locked
            .try_get("crv")
            .map_err(|_| ReviewStoreError::Unavailable)?;
        let state: String = locked
            .try_get("state")
            .map_err(|_| ReviewStoreError::Unavailable)?;
        let qid: String = locked
            .try_get("qid")
            .map_err(|_| ReviewStoreError::Unavailable)?;
        let cid: String = locked
            .try_get("cid")
            .map_err(|_| ReviewStoreError::Unavailable)?;
        let qpid: String = locked
            .try_get("qpid")
            .map_err(|_| ReviewStoreError::Unavailable)?;
        let cpid: String = locked
            .try_get("cpid")
            .map_err(|_| ReviewStoreError::Unavailable)?;
        // Lock unique people in a stable order after case/source rows. If a competing merge
        // wins while this statement waits, the two source records can now point to the same
        // person; locking both joined aliases in one SELECT can make PostgreSQL drop the row
        // during EvalPlanQual. This separate query locks that shared person only once.
        let person_rows = sqlx::query(
            "SELECT person_id::text AS person_id,resource_version FROM litecr.person WHERE person_id IN ($1::uuid,$2::uuid) ORDER BY person_id FOR UPDATE",
        )
        .bind(&qpid)
        .bind(&cpid)
        .fetch_all(&mut *tx)
        .await
        .map_err(|_| ReviewStoreError::Unavailable)?;
        let mut qpv = None;
        let mut cpv = None;
        for row in person_rows {
            let person_id: String = row
                .try_get("person_id")
                .map_err(|_| ReviewStoreError::Unavailable)?;
            let version: i64 = row
                .try_get("resource_version")
                .map_err(|_| ReviewStoreError::Unavailable)?;
            if person_id == qpid {
                qpv = Some(version);
            }
            if person_id == cpid {
                cpv = Some(version);
            }
        }
        let qpv = qpv.ok_or(ReviewStoreError::NotFound)?;
        let cpv = cpv.ok_or(ReviewStoreError::NotFound)?;
        if case_version != c.expected_case_version
            || qrv != c.expected_query_receive_version
            || crv != c.expected_candidate_receive_version
            || qpv != c.expected_query_person_version
            || cpv != c.expected_candidate_person_version
            || (state == "decided" && c.kind != ReviewDecisionKind::ReverseMerge)
        {
            return Err(ReviewStoreError::Conflict);
        }
        let decision_id:String=sqlx::query_scalar("INSERT INTO litecr.review_decision (case_id,case_version,decision,actor_id,actor_issuer_url,reason_code,reason,idempotency_key_hash,command_fingerprint,outcome) VALUES ($1::uuid,$2,$3,$4,$5,$6,$7,$8,$9,'accepted') RETURNING decision_id::text")
            .bind(&c.case_id).bind(case_version).bind(decision_name(&c.kind)).bind(&actor.id).bind(actor.issuer_url.as_deref().unwrap_or("")).bind(&c.reason_code).bind(&c.reason).bind(key.to_vec()).bind(fingerprint.to_vec()).fetch_one(&mut *tx).await.map_err(|_|ReviewStoreError::Unavailable)?;
        let mut merge_id: Option<String> = None;
        match c.kind {
            ReviewDecisionKind::ConfirmedNonMatch => {
                if qpid == cpid {
                    return Err(ReviewStoreError::Conflict);
                }
                let (left, right) = if qid < cid {
                    (&qid, &cid)
                } else {
                    (&cid, &qid)
                };
                sqlx::query("INSERT INTO litecr.confirmed_non_match (registry_id,organization_id,left_source_record_id,right_source_record_id,decision_id) VALUES ($1,$2,$3::uuid,$4::uuid,$5::uuid)")
                    .bind(registry).bind(org).bind(left).bind(right).bind(&decision_id).execute(&mut *tx).await.map_err(|_|ReviewStoreError::Conflict)?;
            }
            ReviewDecisionKind::Link | ReviewDecisionKind::Merge => {
                if qpid == cpid || crossing_non_match(&mut tx, registry, org, &qpid, &cpid).await? {
                    return Err(ReviewStoreError::Conflict);
                }
                if c.kind == ReviewDecisionKind::Link {
                    sqlx::query("INSERT INTO litecr.identity_link (registry_id,organization_id,source_person_id,target_person_id,decision_id) VALUES ($1,$2,$3::uuid,$4::uuid,$5::uuid)").bind(registry).bind(org).bind(&qpid).bind(&cpid).bind(&decision_id).execute(&mut *tx).await.map_err(|_|ReviewStoreError::Unavailable)?;
                    sqlx::query("UPDATE litecr.source_record SET person_id=$1::uuid,updated_at=now() WHERE source_record_id=$2::uuid AND person_id=$3::uuid").bind(&cpid).bind(&qid).bind(&qpid).execute(&mut *tx).await.map_err(|_|ReviewStoreError::Unavailable)?;
                    bump_persons(&mut tx, &qpid, &cpid).await?;
                    event(&mut tx,actor,OutboxEvent{registry,organization:org,aggregate:"person",aggregate_id:&cpid,version:cpv+1,kind:"registry.membership.linked",payload:serde_json::json!({"source_record_id":qid,"target_person_ref":cpid,"decision_id":decision_id})}).await?;
                } else {
                    let mid:String=sqlx::query_scalar("INSERT INTO litecr.identity_merge (registry_id,organization_id,source_person_id,target_person_id,decision_id,source_version_before,target_version_before,source_version_after,target_version_after) VALUES ($1,$2,$3::uuid,$4::uuid,$5::uuid,$6,$7,$6+1,$7+1) RETURNING merge_id::text").bind(registry).bind(org).bind(&qpid).bind(&cpid).bind(&decision_id).bind(qpv).bind(cpv).fetch_one(&mut *tx).await.map_err(|_|ReviewStoreError::Unavailable)?;
                    sqlx::query("INSERT INTO litecr.identity_merge_member (merge_id,source_record_id,receive_version) SELECT $1::uuid,sr.source_record_id,sr.current_receive_version FROM litecr.source_record sr JOIN litecr.source_record_version sv ON sv.source_record_id=sr.source_record_id AND sv.receive_version=sr.current_receive_version WHERE sr.person_id=$2::uuid AND sr.withdrawn_at IS NULL AND sv.withdrawn_at IS NULL").bind(&mid).bind(&qpid).execute(&mut *tx).await.map_err(|_|ReviewStoreError::Unavailable)?;
                    sqlx::query("UPDATE litecr.source_record sr SET person_id=$1::uuid,updated_at=now() FROM litecr.source_record_version sv WHERE sv.source_record_id=sr.source_record_id AND sv.receive_version=sr.current_receive_version AND sr.person_id=$2::uuid AND sr.withdrawn_at IS NULL AND sv.withdrawn_at IS NULL").bind(&cpid).bind(&qpid).execute(&mut *tx).await.map_err(|_|ReviewStoreError::Unavailable)?;
                    sqlx::query("INSERT INTO litecr.person_redirect (registry_id,organization_id,source_person_id,target_person_id,merge_id) VALUES ($1,$2,$3::uuid,$4::uuid,$5::uuid)").bind(registry).bind(org).bind(&qpid).bind(&cpid).bind(&mid).execute(&mut *tx).await.map_err(|_|ReviewStoreError::Unavailable)?;
                    sqlx::query("UPDATE litecr.person SET resource_version=resource_version+1 WHERE person_id IN ($1::uuid,$2::uuid)").bind(&qpid).bind(&cpid).execute(&mut *tx).await.map_err(|_|ReviewStoreError::Unavailable)?;
                    sqlx::query("UPDATE litecr.review_decision SET merge_id=$1::uuid WHERE decision_id=$2::uuid").bind(&mid).bind(&decision_id).execute(&mut *tx).await.map_err(|_|ReviewStoreError::Unavailable)?;
                    merge_id = Some(mid.clone());
                    event(&mut tx,actor,OutboxEvent{registry,organization:org,aggregate:"person",aggregate_id:&cpid,version:cpv+1,kind:"registry.identity.merged",payload:serde_json::json!({"merge_id":mid,"source_person_ref":qpid,"target_person_ref":cpid,"decision_id":decision_id,"reason_code":c.reason_code})}).await?;
                }
            }
            ReviewDecisionKind::ReverseMerge => {
                let mid = c.merge_id.as_deref().ok_or(ReviewStoreError::Conflict)?;
                let m=sqlx::query("SELECT source_person_id::text,target_person_id::text,source_version_after,target_version_after,reversed_by::text FROM litecr.identity_merge WHERE merge_id=$1::uuid AND registry_id=$2 AND organization_id=$3 FOR UPDATE").bind(mid).bind(registry).bind(org).fetch_optional(&mut *tx).await.map_err(|_|ReviewStoreError::Unavailable)?.ok_or(ReviewStoreError::NotFound)?;
                let spid: String = m
                    .try_get("source_person_id")
                    .map_err(|_| ReviewStoreError::Unavailable)?;
                let tpid: String = m
                    .try_get("target_person_id")
                    .map_err(|_| ReviewStoreError::Unavailable)?;
                let case_is_about_merge: bool = sqlx::query_scalar("SELECT $1::uuid=$2::uuid AND $3::uuid=$2::uuid AND EXISTS (SELECT 1 FROM litecr.identity_merge_member mm WHERE mm.merge_id=$4::uuid AND mm.source_record_id IN ($5::uuid,$6::uuid))")
                    .bind(&qpid).bind(&tpid).bind(&cpid).bind(mid).bind(&qid).bind(&cid)
                    .fetch_one(&mut *tx).await.map_err(|_|ReviewStoreError::Unavailable)?;
                if !case_is_about_merge {
                    return Err(ReviewStoreError::Conflict);
                }
                if m.try_get::<Option<String>, _>("reversed_by")
                    .map_err(|_| ReviewStoreError::Unavailable)?
                    .is_some()
                    || spid == tpid
                {
                    return Err(ReviewStoreError::Conflict);
                }
                let changed=sqlx::query("SELECT EXISTS(SELECT 1 FROM litecr.identity_merge_member mm LEFT JOIN litecr.source_record sr ON sr.source_record_id=mm.source_record_id WHERE mm.merge_id=$1::uuid AND (sr.person_id<>$2::uuid OR sr.current_receive_version<>mm.receive_version OR sr.source_record_id IS NULL)) OR EXISTS(SELECT 1 FROM litecr.identity_merge m2 WHERE m2.merge_id<>$1::uuid AND m2.reversed_by IS NULL AND m2.source_person_id=$2::uuid) OR EXISTS(SELECT 1 FROM litecr.identity_merge m JOIN litecr.person ps ON ps.person_id=m.source_person_id JOIN litecr.person pt ON pt.person_id=m.target_person_id WHERE m.merge_id=$1::uuid AND (ps.resource_version<>m.source_version_after OR pt.resource_version<>m.target_version_after))").bind(mid).bind(&tpid).fetch_one(&mut *tx).await.map_err(|_|ReviewStoreError::Unavailable)?;
                let requires: bool = changed
                    .try_get(0)
                    .map_err(|_| ReviewStoreError::Unavailable)?;
                if requires {
                    return Err(ReviewStoreError::RequiresReview);
                }
                if qpv != c.expected_query_person_version
                    || cpv != c.expected_candidate_person_version
                {
                    return Err(ReviewStoreError::Conflict);
                }
                sqlx::query("UPDATE litecr.source_record sr SET person_id=$1::uuid,updated_at=now() FROM litecr.identity_merge_member mm WHERE mm.merge_id=$2::uuid AND mm.source_record_id=sr.source_record_id").bind(&spid).bind(mid).execute(&mut *tx).await.map_err(|_|ReviewStoreError::Unavailable)?;
                sqlx::query(
                    "UPDATE litecr.person_redirect SET active=FALSE WHERE merge_id=$1::uuid",
                )
                .bind(mid)
                .execute(&mut *tx)
                .await
                .map_err(|_| ReviewStoreError::Unavailable)?;
                sqlx::query(
                    "UPDATE litecr.identity_merge SET reversed_by=$1::uuid WHERE merge_id=$2::uuid",
                )
                .bind(&decision_id)
                .bind(mid)
                .execute(&mut *tx)
                .await
                .map_err(|_| ReviewStoreError::Unavailable)?;
                sqlx::query("UPDATE litecr.person SET resource_version=resource_version+1 WHERE person_id IN ($1::uuid,$2::uuid)").bind(&spid).bind(&tpid).execute(&mut *tx).await.map_err(|_|ReviewStoreError::Unavailable)?;
                event(&mut tx,actor,OutboxEvent{registry,organization:org,aggregate:"person",aggregate_id:&tpid,version:cpv+1,kind:"registry.identity.reversed",payload:serde_json::json!({"merge_id":mid,"decision_id":decision_id,"source_person_ref":spid,"target_person_ref":tpid})}).await?;
                merge_id = Some(mid.to_owned());
            }
        }
        sqlx::query("UPDATE litecr.review_case SET state='decided',case_version=case_version+1,updated_at=now() WHERE case_id=$1::uuid").bind(&c.case_id).execute(&mut *tx).await.map_err(|_|ReviewStoreError::Unavailable)?;
        audit(
            &mut tx,
            actor,
            registry,
            org,
            "review.decision.accepted",
            &c.case_id,
            case_version + 1,
        )
        .await?;
        event(&mut tx,actor,OutboxEvent{registry,organization:org,aggregate:"review_case",aggregate_id:&c.case_id,version:case_version+1,kind:"review.case.updated",payload:serde_json::json!({"case_id":c.case_id,"decision_id":decision_id,"decision":decision_name(&c.kind)})}).await?;
        let view = case_view(&mut tx, &c.case_id, registry, org, None).await?;
        tx.commit()
            .await
            .map_err(|_| ReviewStoreError::Unavailable)?;
        Ok(ReviewResult {
            case: view,
            decision_id: Some(decision_id),
            outcome: "accepted".into(),
            merge_id,
            replayed: false,
        })
    }
}

async fn crossing_non_match(
    tx: &mut Transaction<'_, Postgres>,
    registry: &str,
    org: &str,
    a: &str,
    b: &str,
) -> Result<bool, ReviewStoreError> {
    sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM litecr.confirmed_non_match n JOIN litecr.source_record l ON l.source_record_id=n.left_source_record_id JOIN litecr.source_record r ON r.source_record_id=n.right_source_record_id WHERE n.registry_id=$1 AND n.organization_id=$2 AND ((l.person_id=$3::uuid AND r.person_id=$4::uuid) OR (l.person_id=$4::uuid AND r.person_id=$3::uuid)))").bind(registry).bind(org).bind(a).bind(b).fetch_one(&mut **tx).await.map_err(|_|ReviewStoreError::Unavailable)
}
async fn bump_persons(
    tx: &mut Transaction<'_, Postgres>,
    a: &str,
    b: &str,
) -> Result<(), ReviewStoreError> {
    sqlx::query("UPDATE litecr.person SET resource_version=resource_version+1 WHERE person_id IN ($1::uuid,$2::uuid)").bind(a).bind(b).execute(&mut **tx).await.map_err(|_|ReviewStoreError::Unavailable)?;
    Ok(())
}
