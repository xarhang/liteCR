//! First-party PostgreSQL queue adapter for advisory matching.

use async_trait::async_trait;
use litecr_application::worker::{AdvisoryMatchingConsumer, ConsumerCompletion, ConsumerError};
use litecr_domain::worker::{ADVISORY_MATCHER_CONSUMER, ConsumerLease};
use sqlx::{PgPool, Row};

use crate::matching::PostgresMatchingStore;

#[derive(Clone)]
pub struct PostgresMatchingConsumer {
    pool: PgPool,
    matching: PostgresMatchingStore,
}

impl PostgresMatchingConsumer {
    pub fn new(pool: PgPool) -> Self {
        Self {
            matching: PostgresMatchingStore::new(pool.clone()),
            pool,
        }
    }
}

#[async_trait]
impl AdvisoryMatchingConsumer for PostgresMatchingConsumer {
    async fn claim(
        &self,
        owner: &str,
        limit: usize,
        lease_seconds: i64,
        max_attempts: i32,
    ) -> Result<Vec<ConsumerLease>, ConsumerError> {
        if owner.trim().is_empty()
            || owner.len() > 128
            || !(1..=100).contains(&limit)
            || !(5..=900).contains(&lease_seconds)
            || !(1..=12).contains(&max_attempts)
        {
            return Err(ConsumerError::Unavailable);
        }
        sqlx::query(
            "WITH exhausted AS (\
               UPDATE litecr.consumer_job SET state='failed',lease_owner=NULL,lease_token=NULL,lease_expires_at=NULL, \
                 last_error_code='RETRY_EXHAUSTED',updated_at=now() \
               WHERE consumer_name=$1 AND attempts >= $2 \
                 AND (state='pending' OR (state='leased' AND lease_expires_at <= now())) \
               RETURNING consumer_name,event_id \
             ) INSERT INTO litecr.consumer_receipt(consumer_name,event_id,outcome,receipt_code) \
               SELECT consumer_name,event_id,'failed','RETRY_EXHAUSTED' FROM exhausted \
               ON CONFLICT (consumer_name,event_id) DO NOTHING",
        )
        .bind(ADVISORY_MATCHER_CONSUMER)
        .bind(max_attempts)
        .execute(&self.pool)
        .await
        .map_err(|_| ConsumerError::Unavailable)?;
        let rows = sqlx::query(
            "WITH claimable AS (\
               SELECT consumer_name,event_id FROM litecr.consumer_job \
               WHERE consumer_name=$1 AND attempts < $5 \
                 AND ((state='pending' AND available_at <= now()) \
                   OR (state='leased' AND lease_expires_at <= now())) \
               ORDER BY available_at,created_at,event_id \
               FOR UPDATE SKIP LOCKED LIMIT $2 \
             ) UPDATE litecr.consumer_job j SET state='leased', attempts=j.attempts+1, \
                 lease_owner=$3, lease_token=gen_random_uuid(), lease_expires_at=now()+make_interval(secs=>$4::int), \
                 updated_at=now(), last_error_code=NULL \
               FROM claimable c WHERE j.consumer_name=c.consumer_name AND j.event_id=c.event_id \
             RETURNING j.event_id::text AS event_id,j.registry_id,j.organization_id, \
                 j.source_record_id::text AS source_record_id,j.expected_receive_version, \
                 j.attempts,j.lease_owner,j.lease_token::text AS lease_token",
        )
        .bind(ADVISORY_MATCHER_CONSUMER)
        .bind(limit as i64)
        .bind(owner)
        .bind(lease_seconds as i32)
        .bind(max_attempts)
        .fetch_all(&self.pool)
        .await
        .map_err(|_| ConsumerError::Unavailable)?;
        rows.into_iter()
            .map(|row| {
                Ok(ConsumerLease {
                    event_id: row
                        .try_get("event_id")
                        .map_err(|_| ConsumerError::Unavailable)?,
                    registry_id: row
                        .try_get("registry_id")
                        .map_err(|_| ConsumerError::Unavailable)?,
                    organization_id: row
                        .try_get("organization_id")
                        .map_err(|_| ConsumerError::Unavailable)?,
                    source_record_id: row
                        .try_get("source_record_id")
                        .map_err(|_| ConsumerError::Unavailable)?,
                    expected_receive_version: row
                        .try_get("expected_receive_version")
                        .map_err(|_| ConsumerError::Unavailable)?,
                    attempt: row
                        .try_get("attempts")
                        .map_err(|_| ConsumerError::Unavailable)?,
                    lease_owner: row
                        .try_get("lease_owner")
                        .map_err(|_| ConsumerError::Unavailable)?,
                    lease_token: row
                        .try_get("lease_token")
                        .map_err(|_| ConsumerError::Unavailable)?,
                })
            })
            .collect()
    }

    async fn evaluate_and_complete(
        &self,
        lease: &ConsumerLease,
        candidate_limit: usize,
    ) -> Result<ConsumerCompletion, ConsumerError> {
        if !(1..=200).contains(&candidate_limit) {
            return Err(ConsumerError::Unavailable);
        }
        self.matching
            .evaluate_for_consumer(lease, candidate_limit)
            .await
    }

    async fn retry(
        &self,
        lease: &ConsumerLease,
        error_code: &str,
        max_attempts: i32,
        base_delay_seconds: i64,
    ) -> Result<bool, ConsumerError> {
        if !matches!(error_code, "TRANSIENT_DATABASE" | "EVIDENCE_UNAVAILABLE")
            || !(1..=12).contains(&max_attempts)
            || !(1..=300).contains(&base_delay_seconds)
        {
            return Err(ConsumerError::Unavailable);
        }
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|_| ConsumerError::Unavailable)?;
        let row = sqlx::query(
            "SELECT attempts FROM litecr.consumer_job WHERE consumer_name=$1 AND event_id=$2::uuid \
             AND state='leased' AND lease_owner=$3 AND lease_token=$4::uuid AND lease_expires_at > now() FOR UPDATE",
        )
        .bind(ADVISORY_MATCHER_CONSUMER)
        .bind(&lease.event_id)
        .bind(&lease.lease_owner)
        .bind(&lease.lease_token)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|_| ConsumerError::Unavailable)?
        .ok_or(ConsumerError::LeaseLost)?;
        let attempts: i32 = row
            .try_get("attempts")
            .map_err(|_| ConsumerError::Unavailable)?;
        if attempts >= max_attempts {
            sqlx::query(
                "INSERT INTO litecr.consumer_receipt(consumer_name,event_id,outcome,receipt_code) \
                 VALUES($1,$2::uuid,'failed','RETRY_EXHAUSTED') ON CONFLICT DO NOTHING",
            )
            .bind(ADVISORY_MATCHER_CONSUMER)
            .bind(&lease.event_id)
            .execute(&mut *tx)
            .await
            .map_err(|_| ConsumerError::Unavailable)?;
            sqlx::query(
                "UPDATE litecr.consumer_job SET state='failed',lease_owner=NULL,lease_token=NULL,lease_expires_at=NULL, \
                 last_error_code='RETRY_EXHAUSTED',updated_at=now() \
                 WHERE consumer_name=$1 AND event_id=$2::uuid AND state='leased' \
                   AND lease_owner=$3 AND lease_token=$4::uuid",
            )
            .bind(ADVISORY_MATCHER_CONSUMER)
            .bind(&lease.event_id)
            .bind(&lease.lease_owner)
            .bind(&lease.lease_token)
            .execute(&mut *tx)
            .await
            .map_err(|_| ConsumerError::Unavailable)?;
            tx.commit().await.map_err(|_| ConsumerError::Unavailable)?;
            return Ok(false);
        }
        let exponent = attempts.saturating_sub(1).clamp(0, 30);
        let delay = base_delay_seconds
            .saturating_mul(1_i64 << exponent)
            .min(3600);
        sqlx::query(
            "UPDATE litecr.consumer_job SET state='pending',available_at=now()+make_interval(secs=>$4::int), \
             lease_owner=NULL,lease_token=NULL,lease_expires_at=NULL,last_error_code=$5,updated_at=now() \
             WHERE consumer_name=$1 AND event_id=$2::uuid AND state='leased' AND lease_owner=$3 AND lease_token=$6::uuid",
        )
        .bind(ADVISORY_MATCHER_CONSUMER)
        .bind(&lease.event_id)
        .bind(&lease.lease_owner)
        .bind(delay as i32)
        .bind(error_code)
        .bind(&lease.lease_token)
        .execute(&mut *tx)
        .await
        .map_err(|_| ConsumerError::Unavailable)?;
        tx.commit().await.map_err(|_| ConsumerError::Unavailable)?;
        Ok(true)
    }
}
