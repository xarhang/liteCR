//! Human review workflow vocabulary. Decisions are commands for Registry, never matcher output.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewState {
    Open,
    Assigned,
    Decided,
    Closed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenReviewCase {
    pub registry_id: String,
    pub organization_id: String,
    pub query_source_record_id: String,
    pub candidate_source_record_id: String,
    pub expected_query_receive_version: i64,
    pub expected_candidate_receive_version: i64,
    /// Optional for legacy clients; when present it must contain this exact
    /// source pair at these receive versions.
    pub evidence_run_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReviewDecisionKind {
    ConfirmedNonMatch,
    Link,
    Merge,
    ReverseMerge,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewDecisionCommand {
    pub case_id: String,
    pub expected_case_version: i64,
    pub expected_query_receive_version: i64,
    pub expected_candidate_receive_version: i64,
    pub expected_query_person_version: i64,
    pub expected_candidate_person_version: i64,
    pub kind: ReviewDecisionKind,
    pub reason_code: String,
    pub reason: String,
    pub idempotency_key: String,
    pub merge_id: Option<String>,
}

/// Scoped, append-only record of a reviewer action for read-only history views.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewDecisionHistory {
    pub decision_id: String,
    pub decision: String,
    pub outcome: String,
    pub reason_code: String,
    pub reason: String,
    pub actor_id: String,
    pub actor_issuer_url: String,
    pub occurred_at: String,
    pub merge_id: Option<String>,
}

impl OpenReviewCase {
    pub fn validate(&self) -> bool {
        !self.registry_id.trim().is_empty()
            && self.registry_id.len() <= 256
            && !self.organization_id.trim().is_empty()
            && self.organization_id.len() <= 256
            && !self.query_source_record_id.trim().is_empty()
            && self.query_source_record_id.len() <= 128
            && !self.candidate_source_record_id.trim().is_empty()
            && self.candidate_source_record_id.len() <= 128
            && self.query_source_record_id != self.candidate_source_record_id
            && self.expected_query_receive_version > 0
            && self.expected_candidate_receive_version > 0
            && self.evidence_run_id.as_ref().is_none_or(|id| {
                id.len() == 36
                    && id.bytes().enumerate().all(|(i, b)| match i {
                        8 | 13 | 18 | 23 => b == b'-',
                        _ => b.is_ascii_hexdigit(),
                    })
            })
    }
}

impl ReviewDecisionCommand {
    pub fn validate(&self) -> bool {
        !self.case_id.trim().is_empty()
            && self.case_id.len() <= 128
            && self.expected_case_version > 0
            && self.expected_query_receive_version > 0
            && self.expected_candidate_receive_version > 0
            && self.expected_query_person_version > 0
            && self.expected_candidate_person_version > 0
            && !self.reason_code.trim().is_empty()
            && self.reason_code.len() <= 128
            && !self.reason.trim().is_empty()
            && self.reason.len() <= 2000
            && (16..=200).contains(&self.idempotency_key.len())
            && self.idempotency_key.bytes().all(|b| b.is_ascii_graphic())
            && (self.kind == ReviewDecisionKind::ReverseMerge) == self.merge_id.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn cmd() -> OpenReviewCase {
        OpenReviewCase {
            registry_id: "reg-synthetic".into(),
            organization_id: "org-synthetic".into(),
            query_source_record_id: "source-a".into(),
            candidate_source_record_id: "source-b".into(),
            expected_query_receive_version: 1,
            expected_candidate_receive_version: 1,
            evidence_run_id: None,
        }
    }
    #[test]
    fn cases_require_distinct_records_and_positive_versions() {
        assert!(cmd().validate());
        assert!(
            !OpenReviewCase {
                candidate_source_record_id: "source-a".into(),
                ..cmd()
            }
            .validate()
        );
        assert!(
            !OpenReviewCase {
                expected_query_receive_version: 0,
                ..cmd()
            }
            .validate()
        );
        assert!(
            !OpenReviewCase {
                evidence_run_id: Some("not-a-uuid".into()),
                ..cmd()
            }
            .validate()
        );
    }
}
