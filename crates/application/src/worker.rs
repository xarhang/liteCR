//! Narrow application port for the first-party advisory matching consumer.

use async_trait::async_trait;
use litecr_domain::worker::{ConsumerLease, ConsumerReceiptCode};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConsumerCompletion {
    EvidencePersisted { evidence_run_id: String },
    Obsolete { code: ConsumerReceiptCode },
}

#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum ConsumerError {
    #[error("worker lease is no longer current")]
    LeaseLost,
    #[error("source changed while advisory evidence was being computed")]
    StaleSnapshot,
    #[error("worker persistence is temporarily unavailable")]
    Unavailable,
}

/// The single worker capability: it may persist advisory evidence, never mutate registry data.
#[async_trait]
pub trait AdvisoryMatchingConsumer: Send + Sync + 'static {
    async fn claim(
        &self,
        owner: &str,
        limit: usize,
        lease_seconds: i64,
        max_attempts: i32,
    ) -> Result<Vec<ConsumerLease>, ConsumerError>;

    /// Computes bounded evidence and commits its receipt and job completion atomically.
    async fn evaluate_and_complete(
        &self,
        lease: &ConsumerLease,
        candidate_limit: usize,
    ) -> Result<ConsumerCompletion, ConsumerError>;

    async fn retry(
        &self,
        lease: &ConsumerLease,
        error_code: &str,
        max_attempts: i32,
        base_delay_seconds: i64,
    ) -> Result<bool, ConsumerError>;
}
