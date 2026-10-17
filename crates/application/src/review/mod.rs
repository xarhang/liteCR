//! Scoped human adjudication. Registry persistence applies every identity mutation.
use crate::access::{
    AccessContext, AccessDenied, AccessDirectory, AccessDirectoryError, ActorKind, Permission,
    ResourceScope, authorize,
};
use async_trait::async_trait;
use litecr_domain::review::{
    OpenReviewCase, ReviewDecisionCommand, ReviewDecisionHistory, ReviewDecisionKind,
};
use std::{collections::BTreeSet, sync::Arc};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewActor {
    pub id: String,
    pub issuer_url: Option<String>,
    pub request_id: String,
    pub correlation_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewCaseView {
    pub case_id: String,
    pub registry_id: String,
    pub organization_id: String,
    pub query_source_record_id: String,
    pub candidate_source_record_id: String,
    pub evidence_run_id: Option<String>,
    pub query_receive_version: i64,
    pub candidate_receive_version: i64,
    pub query_person_version: i64,
    pub candidate_person_version: i64,
    pub case_version: i64,
    pub state: String,
    pub assigned_actor_id: Option<String>,
    /// Issuer namespace for `assigned_actor_id`; old rows may be unbound.
    pub assigned_actor_issuer_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewResult {
    pub case: ReviewCaseView,
    pub decision_id: Option<String>,
    pub outcome: String,
    pub merge_id: Option<String>,
    pub replayed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewCaseDetail {
    pub case: ReviewCaseView,
    pub history: Vec<ReviewDecisionHistory>,
}

/// Server-derived read boundary passed to the repository so it can avoid
/// loading case history when the actor is neither assigned nor scoped to both
/// source records. Organization-wide grants are represented by `all_records`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewDetailAccess {
    pub actor_id: String,
    pub actor_issuer_url: Option<String>,
    /// Only an authenticated human with a non-empty issuer may use the
    /// assignment exception. Other actor kinds need both record grants.
    pub assigned_case_allowed: bool,
    pub all_records: bool,
    pub record_ids: Vec<String>,
}

impl ReviewDetailAccess {
    pub fn permits(&self, case: &ReviewCaseView) -> bool {
        (self.assigned_case_allowed
            && self
                .actor_issuer_url
                .as_deref()
                .is_some_and(|issuer| !issuer.is_empty())
            && case.assigned_actor_id.as_deref() == Some(self.actor_id.as_str())
            && case.assigned_actor_issuer_url.as_deref() == self.actor_issuer_url.as_deref())
            || self.all_records
            || (self.record_ids.contains(&case.query_source_record_id)
                && self.record_ids.contains(&case.candidate_source_record_id))
    }
}

/// Stable keyset position for a review queue bucket. The ordering fields are
/// immutable creation time plus UUID; the bucket is included so a cursor cannot
/// be reused across Active/Resolved/All lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewQueueFilter {
    All,
    Active,
    Resolved,
}

impl ReviewQueueFilter {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Active => "active",
            Self::Resolved => "resolved",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewCaseCursor {
    pub queue_filter: ReviewQueueFilter,
    pub created_at: String,
    pub case_id: String,
}

impl ReviewCaseCursor {
    pub fn validate(&self) -> bool {
        valid_utc_timestamp(&self.created_at) && valid_uuid(&self.case_id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewCasePage {
    pub cases: Vec<ReviewCaseView>,
    pub has_more: bool,
    pub next_cursor: Option<ReviewCaseCursor>,
}

fn valid_utc_timestamp(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 27
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes[10] != b'T'
        || bytes[13] != b':'
        || bytes[16] != b':'
        || bytes[19] != b'.'
        || bytes[26] != b'Z'
        || ![0..4, 5..7, 8..10, 11..13, 14..16, 17..19, 20..26]
            .into_iter()
            .all(|range| bytes[range].iter().all(u8::is_ascii_digit))
    {
        return false;
    }
    let number = |range: std::ops::Range<usize>| {
        std::str::from_utf8(&bytes[range])
            .ok()
            .and_then(|part| part.parse::<u32>().ok())
            .unwrap_or(u32::MAX)
    };
    let year = number(0..4);
    let month = number(5..7);
    let day = number(8..10);
    let hour = number(11..13);
    let minute = number(14..16);
    let second = number(17..19);
    let days_in_month = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        _ => return false,
    };
    year >= 1 && (1..=days_in_month).contains(&day) && hour <= 23 && minute <= 59 && second <= 59
}

fn valid_uuid(value: &str) -> bool {
    value.len() == 36
        && [8, 13, 18, 23]
            .into_iter()
            .all(|index| value.as_bytes()[index] == b'-')
        && value
            .bytes()
            .enumerate()
            .all(|(index, byte)| [8, 13, 18, 23].contains(&index) || byte.is_ascii_hexdigit())
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ReviewStoreError {
    #[error("review resource not found")]
    NotFound,
    #[error("review state or expected version conflicts with current state")]
    Conflict,
    #[error("reversal depends on later changes and needs human review")]
    RequiresReview,
    #[error("review command is unavailable")]
    Unavailable,
    #[error("matching evidence does not belong to this source pair and receive versions")]
    EvidenceMismatch,
}

#[async_trait]
pub trait ReviewStore: Send + Sync + 'static {
    async fn detail(
        &self,
        case_id: &str,
        registry_id: &str,
        organization_id: &str,
        access: &ReviewDetailAccess,
    ) -> Result<ReviewCaseDetail, ReviewStoreError>;
    async fn list_cases(
        &self,
        registry_id: &str,
        organization_id: &str,
        queue_filter: ReviewQueueFilter,
        limit: u16,
        after: Option<&ReviewCaseCursor>,
    ) -> Result<ReviewCasePage, ReviewStoreError>;
    async fn open_case(
        &self,
        actor: &ReviewActor,
        command: &OpenReviewCase,
    ) -> Result<ReviewCaseView, ReviewStoreError>;
    async fn assign(
        &self,
        actor: &ReviewActor,
        case_id: &str,
        registry_id: &str,
        organization_id: &str,
        expected_case_version: i64,
        assignee_id: &str,
    ) -> Result<ReviewCaseView, ReviewStoreError>;
    async fn decide(
        &self,
        actor: &ReviewActor,
        registry_id: &str,
        organization_id: &str,
        command: &ReviewDecisionCommand,
    ) -> Result<ReviewResult, ReviewStoreError>;
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ReviewError {
    #[error("access denied")]
    AccessDenied(#[from] AccessDenied),
    #[error("review command is invalid")]
    InvalidCommand,
    #[error(transparent)]
    Store(#[from] ReviewStoreError),
}

pub struct ReviewService {
    store: Box<dyn ReviewStore>,
    assignment_directory: Option<Arc<dyn AccessDirectory>>,
}
impl ReviewService {
    pub fn new(store: Box<dyn ReviewStore>) -> Self {
        Self {
            store,
            assignment_directory: None,
        }
    }

    /// Enables recipient validation for assignment commands. Serving
    /// compositions must provide the same directory used by authentication;
    /// without it, assignment fails closed.
    pub fn with_access_directory(
        store: Box<dyn ReviewStore>,
        assignment_directory: Arc<dyn AccessDirectory>,
    ) -> Self {
        Self {
            store,
            assignment_directory: Some(assignment_directory),
        }
    }

    pub async fn detail(
        &self,
        context: Option<&AccessContext>,
        case_id: &str,
        registry_id: &str,
        organization_id: &str,
    ) -> Result<ReviewCaseDetail, ReviewError> {
        if case_id.trim().is_empty()
            || case_id.len() > 128
            || registry_id.trim().is_empty()
            || organization_id.trim().is_empty()
        {
            return Err(ReviewError::InvalidCommand);
        }
        let ctx = context.ok_or(AccessDenied::Unauthenticated)?;
        let in_organization = ctx
            .grants()
            .iter()
            .filter(|grant| {
                grant.registry_id == registry_id && grant.organization_id == organization_id
            })
            .collect::<Vec<_>>();
        if in_organization.is_empty() {
            return Err(AccessDenied::OutOfScope.into());
        }
        let read_grants = in_organization
            .into_iter()
            .filter(|grant| grant.permits(Permission::ReviewRead))
            .collect::<Vec<_>>();

        let mut record_ids = BTreeSet::new();
        let mut all_records = false;
        for grant in read_grants {
            match &grant.record_ids {
                None => all_records = true,
                Some(ids) => record_ids.extend(ids.iter().cloned()),
            }
        }
        let access = ReviewDetailAccess {
            actor_id: ctx.actor().id.clone(),
            actor_issuer_url: ctx.actor().issuer_url.clone(),
            assigned_case_allowed: ctx.actor().kind == ActorKind::Human,
            all_records,
            record_ids: record_ids.into_iter().collect(),
        };

        let detail = self
            .store
            .detail(case_id, registry_id, organization_id, &access)
            .await
            .map_err(ReviewError::from)?;
        if !access.permits(&detail.case) {
            // Conceal cases outside the caller's assignment and record grants.
            return Err(ReviewError::Store(ReviewStoreError::NotFound));
        }
        Ok(detail)
    }

    pub async fn list_cases(
        &self,
        context: Option<&AccessContext>,
        registry_id: &str,
        organization_id: &str,
        queue_filter: ReviewQueueFilter,
        limit: u16,
        after: Option<&ReviewCaseCursor>,
    ) -> Result<ReviewCasePage, ReviewError> {
        if registry_id.trim().is_empty()
            || organization_id.trim().is_empty()
            || limit == 0
            || limit > 100
            || after.is_some_and(|cursor| !cursor.validate() || cursor.queue_filter != queue_filter)
        {
            return Err(ReviewError::InvalidCommand);
        }
        let scope = ResourceScope {
            registry_id: registry_id.to_owned(),
            organization_id: organization_id.to_owned(),
            record_id: None,
        };
        authorize(context, Permission::ReviewRead, &scope)?;
        self.store
            .list_cases(registry_id, organization_id, queue_filter, limit, after)
            .await
            .map_err(Into::into)
    }

    pub async fn open_case(
        &self,
        context: Option<&AccessContext>,
        command: OpenReviewCase,
    ) -> Result<ReviewCaseView, ReviewError> {
        if !command.validate() {
            return Err(ReviewError::InvalidCommand);
        }
        let scope = ResourceScope {
            registry_id: command.registry_id.clone(),
            organization_id: command.organization_id.clone(),
            record_id: Some(command.query_source_record_id.clone()),
        };
        authorize(context, Permission::ReviewDecide, &scope)?;
        let candidate_scope = ResourceScope {
            record_id: Some(command.candidate_source_record_id.clone()),
            ..scope.clone()
        };
        authorize(context, Permission::ReviewDecide, &candidate_scope)?;
        let ctx = context.ok_or(AccessDenied::Unauthenticated)?;
        self.store
            .open_case(&actor(ctx), &command)
            .await
            .map_err(Into::into)
    }

    pub async fn assign(
        &self,
        context: Option<&AccessContext>,
        case_id: &str,
        registry_id: &str,
        organization_id: &str,
        expected_case_version: i64,
        assignee_id: &str,
    ) -> Result<ReviewCaseView, ReviewError> {
        if case_id.trim().is_empty()
            || case_id.len() > 128
            || expected_case_version <= 0
            || assignee_id.trim().is_empty()
            || assignee_id.len() > 512
        {
            return Err(ReviewError::InvalidCommand);
        }
        let scope = ResourceScope {
            registry_id: registry_id.to_owned(),
            organization_id: organization_id.to_owned(),
            record_id: None,
        };
        authorize(context, Permission::ReviewDecide, &scope)?;
        let ctx = context.ok_or(AccessDenied::Unauthenticated)?;
        if ctx.actor().kind != crate::access::ActorKind::Human
            || ctx.actor().issuer_url.as_deref().is_none_or(str::is_empty)
        {
            return Err(AccessDenied::PermissionMissing.into());
        }
        let directory = self
            .assignment_directory
            .as_ref()
            .ok_or(ReviewError::Store(ReviewStoreError::Unavailable))?;
        let recipient = directory
            .load_identity(
                ActorKind::Human,
                ctx.actor().issuer_url.as_deref(),
                assignee_id,
            )
            .await;
        let recipient = match recipient {
            Ok(Some(recipient)) if recipient.active => recipient,
            Ok(None) | Err(AccessDirectoryError::Inactive) => {
                return Err(AccessDenied::PermissionMissing.into());
            }
            Err(AccessDirectoryError::Unavailable) => {
                return Err(ReviewStoreError::Unavailable.into());
            }
            Ok(Some(_)) => return Err(AccessDenied::PermissionMissing.into()),
        };
        let has_scoped_review_decide = recipient.into_scope_grants().iter().any(|grant| {
            grant.registry_id == registry_id
                && grant.organization_id == organization_id
                && grant.record_ids.is_none()
                && grant.permits(Permission::ReviewDecide)
        });
        if !has_scoped_review_decide {
            return Err(AccessDenied::PermissionMissing.into());
        }
        self.store
            .assign(
                &actor(ctx),
                case_id,
                registry_id,
                organization_id,
                expected_case_version,
                assignee_id,
            )
            .await
            .map_err(Into::into)
    }

    pub async fn decide(
        &self,
        context: Option<&AccessContext>,
        registry_id: &str,
        organization_id: &str,
        command: ReviewDecisionCommand,
    ) -> Result<ReviewResult, ReviewError> {
        if !command.validate() {
            return Err(ReviewError::InvalidCommand);
        }
        let scope = ResourceScope {
            registry_id: registry_id.to_owned(),
            organization_id: organization_id.to_owned(),
            record_id: None,
        };
        authorize(context, Permission::ReviewDecide, &scope)?;
        let mutation_permission = match command.kind {
            ReviewDecisionKind::Link => Some(Permission::IdentityLink),
            ReviewDecisionKind::Merge => Some(Permission::IdentityMerge),
            ReviewDecisionKind::ReverseMerge => Some(Permission::IdentityReverse),
            ReviewDecisionKind::ConfirmedNonMatch => None,
        };
        if let Some(permission) = mutation_permission {
            authorize(context, permission, &scope)?;
        }
        let ctx = context.ok_or(AccessDenied::Unauthenticated)?;
        self.store
            .decide(&actor(ctx), registry_id, organization_id, &command)
            .await
            .map_err(Into::into)
    }
}
fn actor(ctx: &AccessContext) -> ReviewActor {
    ReviewActor {
        id: ctx.actor().id.clone(),
        issuer_url: ctx.actor().issuer_url.clone(),
        request_id: ctx.request_id().to_owned(),
        correlation_id: ctx.correlation_id().map(str::to_owned),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::access::{ActorKind, ActorRef, Role, ScopeGrant};
    use std::{
        collections::BTreeSet,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
    };
    struct Store(Arc<AtomicUsize>);
    #[async_trait]
    impl ReviewStore for Store {
        async fn detail(
            &self,
            _: &str,
            _: &str,
            _: &str,
            _: &ReviewDetailAccess,
        ) -> Result<ReviewCaseDetail, ReviewStoreError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Err(ReviewStoreError::Unavailable)
        }
        async fn list_cases(
            &self,
            _: &str,
            _: &str,
            _: ReviewQueueFilter,
            _: u16,
            _: Option<&ReviewCaseCursor>,
        ) -> Result<ReviewCasePage, ReviewStoreError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Err(ReviewStoreError::Unavailable)
        }
        async fn open_case(
            &self,
            _: &ReviewActor,
            _: &OpenReviewCase,
        ) -> Result<ReviewCaseView, ReviewStoreError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Err(ReviewStoreError::Unavailable)
        }
        async fn assign(
            &self,
            _: &ReviewActor,
            _: &str,
            _: &str,
            _: &str,
            _: i64,
            _: &str,
        ) -> Result<ReviewCaseView, ReviewStoreError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Err(ReviewStoreError::Unavailable)
        }
        async fn decide(
            &self,
            _: &ReviewActor,
            _: &str,
            _: &str,
            _: &ReviewDecisionCommand,
        ) -> Result<ReviewResult, ReviewStoreError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Err(ReviewStoreError::Unavailable)
        }
    }
    fn reviewer() -> AccessContext {
        AccessContext::from_authorized_identity(
            ActorRef {
                kind: ActorKind::Human,
                id: "reviewer-synthetic".into(),
                issuer_url: Some("https://issuer.example".into()),
            },
            vec![ScopeGrant {
                registry_id: "reg".into(),
                organization_id: "org".into(),
                record_ids: None,
                roles: BTreeSet::from([Role::Reviewer]),
                permissions: BTreeSet::new(),
            }],
            "req-synthetic".into(),
            None,
            None,
        )
        .unwrap()
    }

    struct DetailStore(ReviewCaseDetail);
    #[async_trait]
    impl ReviewStore for DetailStore {
        async fn detail(
            &self,
            _: &str,
            _: &str,
            _: &str,
            access: &ReviewDetailAccess,
        ) -> Result<ReviewCaseDetail, ReviewStoreError> {
            if access.permits(&self.0.case) {
                Ok(self.0.clone())
            } else {
                Err(ReviewStoreError::NotFound)
            }
        }
        async fn list_cases(
            &self,
            _: &str,
            _: &str,
            _: ReviewQueueFilter,
            _: u16,
            _: Option<&ReviewCaseCursor>,
        ) -> Result<ReviewCasePage, ReviewStoreError> {
            Err(ReviewStoreError::Unavailable)
        }
        async fn open_case(
            &self,
            _: &ReviewActor,
            _: &OpenReviewCase,
        ) -> Result<ReviewCaseView, ReviewStoreError> {
            Err(ReviewStoreError::Unavailable)
        }
        async fn assign(
            &self,
            _: &ReviewActor,
            _: &str,
            _: &str,
            _: &str,
            _: i64,
            _: &str,
        ) -> Result<ReviewCaseView, ReviewStoreError> {
            Err(ReviewStoreError::Unavailable)
        }
        async fn decide(
            &self,
            _: &ReviewActor,
            _: &str,
            _: &str,
            _: &ReviewDecisionCommand,
        ) -> Result<ReviewResult, ReviewStoreError> {
            Err(ReviewStoreError::Unavailable)
        }
    }

    fn detail_case(assigned_actor_id: Option<&str>) -> ReviewCaseDetail {
        ReviewCaseDetail {
            case: ReviewCaseView {
                case_id: "case-1".into(),
                registry_id: "reg".into(),
                organization_id: "org".into(),
                query_source_record_id: "source-a".into(),
                candidate_source_record_id: "source-b".into(),
                evidence_run_id: None,
                query_receive_version: 1,
                candidate_receive_version: 1,
                query_person_version: 1,
                candidate_person_version: 1,
                case_version: 1,
                state: "assigned".into(),
                assigned_actor_id: assigned_actor_id.map(str::to_owned),
                assigned_actor_issuer_url: assigned_actor_id
                    .map(|_| "https://issuer.example".to_owned()),
            },
            history: Vec::new(),
        }
    }

    fn reviewer_with_records(record_ids: BTreeSet<String>) -> AccessContext {
        AccessContext::from_authorized_identity(
            ActorRef {
                kind: ActorKind::Human,
                id: "reviewer-synthetic".into(),
                issuer_url: Some("https://issuer.example".into()),
            },
            vec![ScopeGrant {
                registry_id: "reg".into(),
                organization_id: "org".into(),
                record_ids: Some(record_ids),
                roles: BTreeSet::from([Role::Reviewer]),
                permissions: BTreeSet::new(),
            }],
            "req-synthetic".into(),
            None,
            None,
        )
        .unwrap()
    }

    fn reviewer_with_issuer(issuer_url: &str) -> AccessContext {
        AccessContext::from_authorized_identity(
            ActorRef {
                kind: ActorKind::Human,
                id: "reviewer-synthetic".into(),
                issuer_url: Some(issuer_url.into()),
            },
            vec![ScopeGrant {
                registry_id: "reg".into(),
                organization_id: "org".into(),
                record_ids: Some(BTreeSet::new()),
                roles: BTreeSet::from([Role::Reviewer]),
                permissions: BTreeSet::new(),
            }],
            "req-synthetic".into(),
            None,
            None,
        )
        .unwrap()
    }

    fn registrar_without_review_read() -> AccessContext {
        AccessContext::from_authorized_identity(
            ActorRef {
                kind: ActorKind::Human,
                id: "reviewer-synthetic".into(),
                issuer_url: Some("https://issuer.example".into()),
            },
            vec![ScopeGrant {
                registry_id: "reg".into(),
                organization_id: "org".into(),
                record_ids: Some(BTreeSet::new()),
                roles: BTreeSet::from([Role::Registrar]),
                permissions: BTreeSet::new(),
            }],
            "req-synthetic".into(),
            None,
            None,
        )
        .unwrap()
    }

    fn integration_client_with_records(
        record_ids: BTreeSet<String>,
        permissions: BTreeSet<Permission>,
    ) -> AccessContext {
        AccessContext::from_authorized_identity(
            ActorRef {
                kind: ActorKind::IntegrationClient,
                // Deliberately collide with the assigned human's issuer and
                // subject to prove that actor kind is part of the boundary.
                id: "reviewer-synthetic".into(),
                issuer_url: Some("https://issuer.example".into()),
            },
            vec![ScopeGrant {
                registry_id: "reg".into(),
                organization_id: "org".into(),
                record_ids: Some(record_ids),
                roles: BTreeSet::from([Role::IntegrationClient]),
                permissions,
            }],
            "req-integration-synthetic".into(),
            None,
            None,
        )
        .unwrap()
    }

    #[tokio::test]
    async fn review_detail_requires_assignment_or_grants_for_both_source_records() {
        let assigned = ReviewService::new(Box::new(DetailStore(detail_case(Some(
            "reviewer-synthetic",
        )))));
        assert!(
            assigned
                .detail(
                    Some(&reviewer_with_records(BTreeSet::from(["source-a".into()]))),
                    "case-1",
                    "reg",
                    "org",
                )
                .await
                .is_ok()
        );

        // An integration identity can share issuer+subject text with the
        // assigned human but cannot use the human assignment exception.
        let integration_without_read =
            integration_client_with_records(BTreeSet::new(), BTreeSet::new());
        assert!(matches!(
            assigned
                .detail(Some(&integration_without_read), "case-1", "reg", "org",)
                .await,
            Err(ReviewError::Store(ReviewStoreError::NotFound))
        ));

        // Actor kind does not bypass the explicitly selected both-record
        // grant path.
        let integration_with_both = integration_client_with_records(
            BTreeSet::from(["source-a".into(), "source-b".into()]),
            BTreeSet::from([Permission::ReviewRead]),
        );
        assert!(
            assigned
                .detail(Some(&integration_with_both), "case-1", "reg", "org",)
                .await
                .is_ok()
        );

        // Assignment is its own detail-read path, even when the assigned
        // actor's organization grant does not include ReviewRead.
        assert!(
            assigned
                .detail(
                    Some(&registrar_without_review_read()),
                    "case-1",
                    "reg",
                    "org",
                )
                .await
                .is_ok()
        );

        assert!(matches!(
            assigned
                .detail(
                    Some(&reviewer_with_issuer("https://other-issuer.example")),
                    "case-1",
                    "reg",
                    "org"
                )
                .await,
            Err(ReviewError::Store(ReviewStoreError::NotFound))
        ));

        let both_records = ReviewService::new(Box::new(DetailStore(detail_case(None))));
        let context = reviewer_with_records(BTreeSet::from(["source-a".into(), "source-b".into()]));
        assert!(
            both_records
                .detail(Some(&context), "case-1", "reg", "org")
                .await
                .is_ok()
        );

        let one_record = reviewer_with_records(BTreeSet::from(["source-a".into()]));
        assert!(matches!(
            both_records
                .detail(Some(&one_record), "case-1", "reg", "org")
                .await,
            Err(ReviewError::Store(ReviewStoreError::NotFound))
        ));

        assert!(matches!(
            both_records
                .detail(
                    Some(&registrar_without_review_read()),
                    "case-1",
                    "reg",
                    "org"
                )
                .await,
            Err(ReviewError::Store(ReviewStoreError::NotFound))
        ));
    }

    #[test]
    fn review_cursor_accepts_canonical_generated_position_and_rejects_tampering() {
        let valid = ReviewCaseCursor {
            queue_filter: ReviewQueueFilter::Active,
            created_at: "2026-10-09T12:34:56.123456Z".into(),
            case_id: "123e4567-e89b-12d3-a456-426614174000".into(),
        };
        assert!(valid.validate());
        assert!(
            !ReviewCaseCursor {
                created_at: "2026-99-09T12:34:56.123456Z".into(),
                ..valid.clone()
            }
            .validate()
        );
        assert!(
            !ReviewCaseCursor {
                created_at: "0000-02-29T12:34:56.123456Z".into(),
                ..valid.clone()
            }
            .validate()
        );
        assert!(
            !ReviewCaseCursor {
                case_id: "not-a-uuid".into(),
                ..valid
            }
            .validate()
        );
    }
    #[tokio::test]
    async fn scope_and_role_are_checked_before_review_storage() {
        let calls = Arc::new(AtomicUsize::new(0));
        let svc = ReviewService::new(Box::new(Store(calls.clone())));
        let c = OpenReviewCase {
            registry_id: "reg".into(),
            organization_id: "other".into(),
            query_source_record_id: "a".into(),
            candidate_source_record_id: "b".into(),
            expected_query_receive_version: 1,
            expected_candidate_receive_version: 1,
            evidence_run_id: None,
        };
        assert!(matches!(
            svc.open_case(Some(&reviewer()), c).await,
            Err(ReviewError::AccessDenied(_))
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn assignment_fails_closed_without_recipient_directory() {
        let calls = Arc::new(AtomicUsize::new(0));
        let service = ReviewService::new(Box::new(Store(calls.clone())));

        assert!(matches!(
            service
                .assign(
                    Some(&reviewer()),
                    "case-1",
                    "reg",
                    "org",
                    1,
                    "recipient-synthetic",
                )
                .await,
            Err(ReviewError::Store(ReviewStoreError::Unavailable))
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn malformed_review_cursor_is_rejected_before_storage() {
        let calls = Arc::new(AtomicUsize::new(0));
        let service = ReviewService::new(Box::new(Store(calls.clone())));
        let cursor = ReviewCaseCursor {
            queue_filter: ReviewQueueFilter::Active,
            created_at: "2026-10-09T12:34:56.123456Z".into(),
            case_id: "not-a-uuid".into(),
        };
        assert!(matches!(
            service
                .list_cases(
                    Some(&reviewer()),
                    "reg",
                    "org",
                    ReviewQueueFilter::Active,
                    50,
                    Some(&cursor),
                )
                .await,
            Err(ReviewError::InvalidCommand)
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn merge_permission_is_required_in_addition_to_review_decide() {
        let calls = Arc::new(AtomicUsize::new(0));
        let service = ReviewService::new(Box::new(Store(calls.clone())));
        let limited = AccessContext::from_authorized_identity(
            ActorRef {
                kind: ActorKind::Human,
                id: "limited-reviewer".into(),
                issuer_url: Some("https://issuer.example".into()),
            },
            vec![ScopeGrant {
                registry_id: "reg".into(),
                organization_id: "org".into(),
                record_ids: None,
                roles: BTreeSet::new(),
                permissions: BTreeSet::from([Permission::ReviewDecide]),
            }],
            "req-limited".into(),
            None,
            None,
        )
        .unwrap();
        let command = ReviewDecisionCommand {
            case_id: "case-synthetic".into(),
            expected_case_version: 1,
            expected_query_receive_version: 1,
            expected_candidate_receive_version: 1,
            expected_query_person_version: 1,
            expected_candidate_person_version: 1,
            kind: ReviewDecisionKind::Merge,
            reason_code: "synthetic".into(),
            reason: "synthetic permission check".into(),
            idempotency_key: "synthetic-idempotency-key".into(),
            merge_id: None,
        };
        assert!(matches!(
            service.decide(Some(&limited), "reg", "org", command).await,
            Err(ReviewError::AccessDenied(_))
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
}
