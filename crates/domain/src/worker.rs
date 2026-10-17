//! Durable advisory-matching consumer job values.

pub const ADVISORY_MATCHER_CONSUMER: &str = "advisory-matcher-v1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsumerLease {
    pub event_id: String,
    pub registry_id: String,
    pub organization_id: String,
    pub source_record_id: String,
    pub expected_receive_version: i64,
    pub attempt: i32,
    pub lease_owner: String,
    pub lease_token: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsumerReceiptCode {
    SourceSuperseded,
    SourceWithdrawn,
    SourceScopeMismatch,
    SourceMalformed,
    RetryExhausted,
}

impl ConsumerReceiptCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SourceSuperseded => "SOURCE_SUPERSEDED",
            Self::SourceWithdrawn => "SOURCE_WITHDRAWN",
            Self::SourceScopeMismatch => "SOURCE_SCOPE_MISMATCH",
            Self::SourceMalformed => "SOURCE_MALFORMED",
            Self::RetryExhausted => "RETRY_EXHAUSTED",
        }
    }
}
