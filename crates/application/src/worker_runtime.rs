//! Bounded orchestration for the first-party advisory matching consumer.
//!
//! This module deliberately knows only the consumer port. It never mutates
//! registry records; lease fencing and atomic evidence/receipt persistence are
//! owned by the infrastructure adapter.

use crate::worker::{AdvisoryMatchingConsumer, ConsumerCompletion, ConsumerError};
use litecr_domain::worker::ConsumerLease;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkerConfig {
    pub claim_limit: usize,
    pub lease_seconds: i64,
    pub candidate_limit: usize,
    pub max_attempts: i32,
    pub retry_base_delay_seconds: i64,
}

impl Default for WorkerConfig {
    fn default() -> Self {
        Self {
            // One at a time keeps shutdown latency bounded and avoids an
            // unbounded pile of leases while matching is CPU/database bound.
            claim_limit: 1,
            lease_seconds: 60,
            candidate_limit: 200,
            max_attempts: 12,
            retry_base_delay_seconds: 5,
        }
    }
}

impl WorkerConfig {
    pub fn validate(self) -> Result<Self, &'static str> {
        if !(1..=100).contains(&self.claim_limit) {
            return Err("worker claim limit must be between 1 and 100");
        }
        if !(5..=900).contains(&self.lease_seconds) {
            return Err("worker lease must be between 5 and 900 seconds");
        }
        if !(1..=200).contains(&self.candidate_limit) {
            return Err("worker candidate limit must be between 1 and 200");
        }
        if !(1..=12).contains(&self.max_attempts) {
            return Err("worker max attempts must be between 1 and 12");
        }
        if !(1..=300).contains(&self.retry_base_delay_seconds) {
            return Err("worker retry delay must be between 1 and 300 seconds");
        }
        Ok(self)
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct BatchReport {
    pub claimed: usize,
    pub completed: usize,
    pub obsolete: usize,
    pub retried: usize,
    pub retry_exhausted: usize,
    pub lease_lost: usize,
    pub deferred_until_lease_expiry: usize,
}

/// Claim at most the configured bound and finish every returned lease before
/// returning. Callers must stop invoking this function once shutdown is seen.
pub async fn run_batch<C: AdvisoryMatchingConsumer>(
    consumer: &C,
    owner: &str,
    config: WorkerConfig,
) -> Result<BatchReport, &'static str> {
    let config = config.validate()?;
    if owner.trim().is_empty() || owner.len() > 128 {
        return Err("worker owner must contain 1 to 128 characters");
    }
    let leases = consumer
        .claim(
            owner,
            config.claim_limit,
            config.lease_seconds,
            config.max_attempts,
        )
        .await
        .map_err(|_| "worker queue claim failed")?;
    if leases.len() > config.claim_limit {
        return Err("worker queue returned more leases than requested");
    }
    let mut report = BatchReport {
        claimed: leases.len(),
        ..BatchReport::default()
    };
    for lease in leases {
        process_lease(consumer, &lease, config, &mut report).await;
    }
    Ok(report)
}

async fn process_lease<C: AdvisoryMatchingConsumer>(
    consumer: &C,
    lease: &ConsumerLease,
    config: WorkerConfig,
    report: &mut BatchReport,
) {
    match consumer
        .evaluate_and_complete(lease, config.candidate_limit)
        .await
    {
        Ok(ConsumerCompletion::EvidencePersisted { .. }) => report.completed += 1,
        Ok(ConsumerCompletion::Obsolete { .. }) => report.obsolete += 1,
        Err(ConsumerError::LeaseLost) => report.lease_lost += 1,
        // A stale source snapshot cannot be retried against the same event.
        // Leave the fenced lease to expire; queue recovery makes the bounded
        // attempt count terminal without allowing a stale write.
        Err(ConsumerError::StaleSnapshot) => report.deferred_until_lease_expiry += 1,
        Err(ConsumerError::Unavailable) => match consumer
            .retry(
                lease,
                "EVIDENCE_UNAVAILABLE",
                config.max_attempts,
                config.retry_base_delay_seconds,
            )
            .await
        {
            Ok(true) => report.retried += 1,
            Ok(false) => report.retry_exhausted += 1,
            Err(ConsumerError::LeaseLost) => report.lease_lost += 1,
            Err(_) => report.deferred_until_lease_expiry += 1,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use litecr_domain::worker::ConsumerLease;
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct FakeState {
        claimed: usize,
        claim_max_attempts: Option<i32>,
        evaluated: usize,
        retries: usize,
        retry_result: bool,
        failures: Vec<ConsumerError>,
    }

    struct FakeConsumer(Arc<Mutex<FakeState>>);

    #[async_trait]
    impl AdvisoryMatchingConsumer for FakeConsumer {
        async fn claim(
            &self,
            _owner: &str,
            limit: usize,
            _lease_seconds: i64,
            max_attempts: i32,
        ) -> Result<Vec<ConsumerLease>, ConsumerError> {
            let mut state = self.0.lock().unwrap();
            state.claimed += 1;
            state.claim_max_attempts = Some(max_attempts);
            Ok((0..limit).map(lease).collect())
        }

        async fn evaluate_and_complete(
            &self,
            _lease: &ConsumerLease,
            _candidate_limit: usize,
        ) -> Result<ConsumerCompletion, ConsumerError> {
            let mut state = self.0.lock().unwrap();
            state.evaluated += 1;
            if state.failures.is_empty() {
                Ok(ConsumerCompletion::EvidencePersisted {
                    evidence_run_id: "synthetic-run".to_owned(),
                })
            } else {
                Err(state.failures.remove(0))
            }
        }

        async fn retry(
            &self,
            _lease: &ConsumerLease,
            _error_code: &str,
            _max_attempts: i32,
            _base_delay_seconds: i64,
        ) -> Result<bool, ConsumerError> {
            let mut state = self.0.lock().unwrap();
            state.retries += 1;
            Ok(state.retry_result)
        }
    }

    fn lease(index: usize) -> ConsumerLease {
        ConsumerLease {
            event_id: format!("synthetic-event-{index}"),
            registry_id: "synthetic-registry".to_owned(),
            organization_id: "synthetic-org".to_owned(),
            source_record_id: format!("synthetic-source-{index}"),
            expected_receive_version: 1,
            attempt: 1,
            lease_owner: "synthetic-worker".to_owned(),
            lease_token: format!("synthetic-token-{index}"),
        }
    }

    #[tokio::test]
    async fn drains_every_claimed_lease_before_returning_and_does_not_claim_again_itself() {
        let state = Arc::new(Mutex::new(FakeState::default()));
        let consumer = FakeConsumer(state.clone());
        let report = run_batch(
            &consumer,
            "synthetic-worker",
            WorkerConfig {
                claim_limit: 3,
                ..WorkerConfig::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(report.claimed, 3);
        assert_eq!(report.completed, 3);
        let state = state.lock().unwrap();
        assert_eq!(state.claimed, 1);
        assert_eq!(state.evaluated, 3);
    }

    #[tokio::test]
    async fn transient_failure_uses_fenced_retry_port_and_records_exhaustion() {
        let state = Arc::new(Mutex::new(FakeState {
            failures: vec![ConsumerError::Unavailable, ConsumerError::Unavailable],
            retry_result: false,
            ..FakeState::default()
        }));
        let consumer = FakeConsumer(state.clone());
        let report = run_batch(
            &consumer,
            "synthetic-worker",
            WorkerConfig {
                max_attempts: 3,
                ..WorkerConfig::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(report.retry_exhausted, 1);
        let state = state.lock().unwrap();
        assert_eq!(state.retries, 1);
        assert_eq!(state.claim_max_attempts, Some(3));
    }

    #[tokio::test]
    async fn stale_snapshot_is_not_retried_and_lost_lease_is_not_persisted() {
        let state = Arc::new(Mutex::new(FakeState {
            failures: vec![ConsumerError::StaleSnapshot],
            ..FakeState::default()
        }));
        let report = run_batch(
            &FakeConsumer(state.clone()),
            "synthetic-worker",
            WorkerConfig::default(),
        )
        .await
        .unwrap();
        assert_eq!(report.deferred_until_lease_expiry, 1);
        assert_eq!(state.lock().unwrap().retries, 0);

        let state = Arc::new(Mutex::new(FakeState {
            failures: vec![ConsumerError::LeaseLost],
            ..FakeState::default()
        }));
        let report = run_batch(
            &FakeConsumer(state.clone()),
            "synthetic-worker",
            WorkerConfig::default(),
        )
        .await
        .unwrap();
        assert_eq!(report.lease_lost, 1);
        assert_eq!(state.lock().unwrap().retries, 0);
    }

    #[test]
    fn config_enforces_database_adapter_bounds() {
        assert!(
            WorkerConfig {
                claim_limit: 0,
                ..WorkerConfig::default()
            }
            .validate()
            .is_err()
        );
        assert!(WorkerConfig::default().validate().is_ok());
    }
}
