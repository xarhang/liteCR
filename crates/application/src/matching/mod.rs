//! Scoped advisory matching use case; Registry remains the mutation authority.
use async_trait::async_trait;
use litecr_matching::MatchResult;
use thiserror::Error;

use crate::access::{AccessContext, AccessDenied, Permission, ResourceScope, authorize};
use sha2::{Digest, Sha256};

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum MatchingStoreError {
    #[error("matching source record was not found")]
    NotFound,
    #[error("matching evidence run was not found in the requested scope")]
    EvidenceNotFound,
    #[error("matching source snapshot changed before evidence was stored")]
    StaleSnapshot,
    #[error("matching scope exceeds the bounded input size")]
    TooManyRecords,
    #[error("matching persistence or source lookup is unavailable")]
    Unavailable,
    #[error("idempotency key was reused with different request parameters")]
    IdempotencyConflict,
}

#[async_trait]
pub trait MatchingStore: Send + Sync + 'static {
    // Keep scope, source, actor, request and optional replay digests explicit at
    // this persistence boundary; regrouping them would churn the store contract.
    #[allow(clippy::too_many_arguments)]
    async fn evaluate(
        &self,
        registry_id: &str,
        organization_id: &str,
        query_source_record_id: &str,
        candidate_limit: usize,
        actor_id: &str,
        request_id: &str,
        idempotency: Option<MatchIdempotency>,
    ) -> Result<MatchResult, MatchingStoreError>;

    async fn read_evidence(
        &self,
        evidence_run_id: &str,
        registry_id: &str,
        organization_id: &str,
    ) -> Result<MatchResult, MatchingStoreError>;
}

/// Digests used to replay a durable match-evidence result without retaining the
/// caller's key or request body. Actor and parameter digests are bound to the
/// authorized request by `MatchingService`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MatchIdempotency {
    pub actor_digest: [u8; 32],
    pub key_digest: [u8; 32],
    pub request_digest: [u8; 32],
}

#[derive(Debug, Error)]
pub enum MatchingError {
    #[error("access denied")]
    AccessDenied(#[from] AccessDenied),
    #[error("invalid matching request")]
    InvalidRequest,
    #[error("invalid idempotency key")]
    InvalidIdempotencyKey,
    #[error(transparent)]
    Store(#[from] MatchingStoreError),
}

pub struct MatchingService {
    store: Box<dyn MatchingStore>,
    max_candidates: usize,
}

impl MatchingService {
    pub fn new(store: Box<dyn MatchingStore>, max_candidates: usize) -> Option<Self> {
        (1..=200).contains(&max_candidates).then_some(Self {
            store,
            max_candidates,
        })
    }

    pub async fn evaluate(
        &self,
        context: Option<&AccessContext>,
        registry_id: &str,
        organization_id: &str,
        query_source_record_id: &str,
        candidate_limit: usize,
        idempotency_key: Option<&str>,
    ) -> Result<MatchResult, MatchingError> {
        if registry_id.trim().is_empty()
            || organization_id.trim().is_empty()
            || query_source_record_id.trim().is_empty()
            || query_source_record_id.len() > 128
            || candidate_limit == 0
            || candidate_limit > self.max_candidates
        {
            return Err(MatchingError::InvalidRequest);
        }
        let scope = ResourceScope {
            registry_id: registry_id.to_owned(),
            organization_id: organization_id.to_owned(),
            record_id: Some(query_source_record_id.to_owned()),
        };
        authorize(context, Permission::MatchRequest, &scope)?;
        let context = context.ok_or(AccessDenied::Unauthenticated)?;
        let idempotency = match idempotency_key {
            Some(key) if valid_idempotency_key(key) => Some(MatchIdempotency {
                actor_digest: actor_digest(context),
                key_digest: sha256_domain(b"litecr.match.idempotency-key.v1", &[key.as_bytes()]),
                request_digest: request_digest(
                    registry_id,
                    organization_id,
                    query_source_record_id,
                    candidate_limit,
                ),
            }),
            Some(_) => return Err(MatchingError::InvalidIdempotencyKey),
            None => None,
        };
        self.store
            .evaluate(
                registry_id,
                organization_id,
                query_source_record_id,
                candidate_limit,
                &context.actor().id,
                context.request_id(),
                idempotency,
            )
            .await
            .map_err(Into::into)
    }

    pub async fn read_evidence(
        &self,
        context: Option<&AccessContext>,
        registry_id: &str,
        organization_id: &str,
        evidence_run_id: &str,
    ) -> Result<MatchResult, MatchingError> {
        if registry_id.trim().is_empty()
            || organization_id.trim().is_empty()
            || !is_uuid(evidence_run_id)
        {
            return Err(MatchingError::InvalidRequest);
        }
        let context = context.ok_or(AccessDenied::Unauthenticated)?;
        // Check that the caller has the read permission for this organization
        // before looking up the opaque run ID. Record-limited grants are checked
        // after a scope-filtered lookup and hide non-authorized record IDs as 404.
        let has_org_permission = context.grants().iter().any(|grant| {
            grant.registry_id == registry_id
                && grant.organization_id == organization_id
                && grant.permits(Permission::MatchEvidenceRead)
        });
        if !has_org_permission {
            if context.grants().iter().any(|grant| {
                grant.registry_id == registry_id && grant.organization_id == organization_id
            }) {
                return Err(MatchingError::AccessDenied(AccessDenied::PermissionMissing));
            }
            return Err(MatchingError::Store(MatchingStoreError::EvidenceNotFound));
        }

        let result = self
            .store
            .read_evidence(evidence_run_id, registry_id, organization_id)
            .await?;
        if !result
            .evidence_run_id
            .as_deref()
            .is_some_and(|stored_id| stored_id.eq_ignore_ascii_case(evidence_run_id))
        {
            return Err(MatchingError::Store(MatchingStoreError::EvidenceNotFound));
        }
        let scope = ResourceScope {
            registry_id: registry_id.to_owned(),
            organization_id: organization_id.to_owned(),
            record_id: Some(result.query_record_id.clone()),
        };
        if context.grants().iter().any(|grant| {
            grant.permits_scope(&scope) && grant.permits(Permission::MatchEvidenceRead)
        }) {
            Ok(result)
        } else {
            Err(MatchingError::Store(MatchingStoreError::EvidenceNotFound))
        }
    }
}

fn valid_idempotency_key(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~'))
}

fn actor_digest(context: &AccessContext) -> [u8; 32] {
    let actor = context.actor();
    let kind = match actor.kind {
        crate::access::ActorKind::Human => b"human".as_slice(),
        crate::access::ActorKind::IntegrationClient => b"integration_client".as_slice(),
        crate::access::ActorKind::System => b"system".as_slice(),
    };
    sha256_domain(
        b"litecr.match.actor.v1",
        &[
            kind,
            actor.issuer_url.as_deref().unwrap_or("").as_bytes(),
            actor.id.as_bytes(),
        ],
    )
}

fn request_digest(
    registry_id: &str,
    organization_id: &str,
    query_source_record_id: &str,
    candidate_limit: usize,
) -> [u8; 32] {
    let candidate_limit = (candidate_limit as u64).to_be_bytes();
    sha256_domain(
        b"litecr.match.request.v1",
        &[
            registry_id.as_bytes(),
            organization_id.as_bytes(),
            query_source_record_id.as_bytes(),
            &candidate_limit,
        ],
    )
}

fn sha256_domain(domain: &[u8], parts: &[&[u8]]) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update((domain.len() as u64).to_be_bytes());
    digest.update(domain);
    for part in parts {
        digest.update((part.len() as u64).to_be_bytes());
        digest.update(part);
    }
    digest.finalize().into()
}

fn is_uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => byte == b'-',
            _ => byte.is_ascii_hexdigit(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::access::{ActorKind, ActorRef, Role, ScopeGrant};
    use litecr_matching::{MatcherVersions, RetrievalTelemetry};
    use std::{
        collections::BTreeSet,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
    };

    struct Store(Arc<AtomicUsize>);
    #[async_trait]
    impl MatchingStore for Store {
        async fn evaluate(
            &self,
            _: &str,
            _: &str,
            _: &str,
            _: usize,
            _: &str,
            _: &str,
            _: Option<MatchIdempotency>,
        ) -> Result<MatchResult, MatchingStoreError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Err(MatchingStoreError::NotFound)
        }

        async fn read_evidence(
            &self,
            _: &str,
            _: &str,
            _: &str,
        ) -> Result<MatchResult, MatchingStoreError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Err(MatchingStoreError::EvidenceNotFound)
        }
    }
    fn context() -> AccessContext {
        AccessContext::from_authorized_identity(
            ActorRef {
                kind: ActorKind::Human,
                id: "synthetic-registrar".into(),
                issuer_url: Some("https://issuer.example".into()),
            },
            vec![ScopeGrant {
                registry_id: "r".into(),
                organization_id: "o".into(),
                record_ids: None,
                roles: BTreeSet::from([Role::Registrar]),
                permissions: BTreeSet::new(),
            }],
            "request-synthetic".into(),
            None,
            None,
        )
        .unwrap()
    }

    fn reviewer_context(record_ids: Option<BTreeSet<String>>) -> AccessContext {
        AccessContext::from_authorized_identity(
            ActorRef {
                kind: ActorKind::Human,
                id: "synthetic-reviewer".into(),
                issuer_url: Some("https://issuer.example".into()),
            },
            vec![ScopeGrant {
                registry_id: "r".into(),
                organization_id: "o".into(),
                record_ids,
                roles: BTreeSet::from([Role::Reviewer]),
                permissions: BTreeSet::new(),
            }],
            "request-synthetic".into(),
            None,
            None,
        )
        .unwrap()
    }

    fn result() -> MatchResult {
        MatchResult {
            versions: MatcherVersions {
                engine: "engine-1".into(),
                normalization: "norm-1".into(),
                candidate: "candidate-1".into(),
                feature: "feature-1".into(),
                policy: "policy-1".into(),
            },
            evidence_run_id: Some("00000000-0000-0000-0000-000000000001".into()),
            query_record_id: "query-1".into(),
            query_source_version: Some("source-1".into()),
            query_receive_version: Some(2),
            candidates: vec![],
            retrieval: RetrievalTelemetry {
                per_pass_candidates: Default::default(),
                union_before_limit: 0,
                returned_candidates: 0,
                truncated: false,
                limit: 20,
                complete: true,
            },
            mutation_authority: "registry-only".into(),
            snapshot_complete: true,
        }
    }

    struct EvidenceStore {
        calls: Arc<AtomicUsize>,
        result: MatchResult,
    }
    #[async_trait]
    impl MatchingStore for EvidenceStore {
        async fn evaluate(
            &self,
            _: &str,
            _: &str,
            _: &str,
            _: usize,
            _: &str,
            _: &str,
            _: Option<MatchIdempotency>,
        ) -> Result<MatchResult, MatchingStoreError> {
            Err(MatchingStoreError::Unavailable)
        }

        async fn read_evidence(
            &self,
            _: &str,
            _: &str,
            _: &str,
        ) -> Result<MatchResult, MatchingStoreError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(self.result.clone())
        }
    }
    #[tokio::test]
    async fn scope_and_permission_are_checked_before_store_access() {
        let calls = Arc::new(AtomicUsize::new(0));
        let service = MatchingService::new(Box::new(Store(calls.clone())), 100).unwrap();
        assert!(matches!(
            service.evaluate(None, "r", "o", "source-1", 10, None).await,
            Err(MatchingError::AccessDenied(_))
        ));
        assert!(matches!(
            service
                .evaluate(Some(&context()), "r", "other-org", "source-1", 10, None)
                .await,
            Err(MatchingError::AccessDenied(_))
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert!(matches!(
            service
                .evaluate(Some(&context()), "r", "o", "source-1", 10, None)
                .await,
            Err(MatchingError::Store(MatchingStoreError::NotFound))
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
    #[tokio::test]
    async fn input_and_limits_are_bounded_before_store_access() {
        let calls = Arc::new(AtomicUsize::new(0));
        let service = MatchingService::new(Box::new(Store(calls.clone())), 100).unwrap();
        assert!(matches!(
            service
                .evaluate(Some(&context()), "r", "o", "source", 101, None)
                .await,
            Err(MatchingError::InvalidRequest)
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn idempotency_key_is_validated_after_authorization_and_before_storage() {
        let calls = Arc::new(AtomicUsize::new(0));
        let service = MatchingService::new(Box::new(Store(calls.clone())), 100).unwrap();
        assert!(matches!(
            service
                .evaluate(Some(&context()), "r", "o", "source-1", 10, Some("bad key"))
                .await,
            Err(MatchingError::InvalidIdempotencyKey)
        ));
        assert!(matches!(
            service
                .evaluate(None, "r", "o", "source-1", 10, Some("bad key"))
                .await,
            Err(MatchingError::AccessDenied(_))
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn idempotency_digests_bind_actor_and_all_request_parameters() {
        let actor = actor_digest(&context());
        let same_subject_different_issuer = AccessContext::from_authorized_identity(
            ActorRef {
                kind: ActorKind::Human,
                id: "synthetic-registrar".into(),
                issuer_url: Some("https://other-issuer.example".into()),
            },
            vec![ScopeGrant {
                registry_id: "r".into(),
                organization_id: "o".into(),
                record_ids: None,
                roles: BTreeSet::from([Role::Registrar]),
                permissions: BTreeSet::new(),
            }],
            "request-synthetic".into(),
            None,
            None,
        )
        .unwrap();
        assert_ne!(actor, actor_digest(&same_subject_different_issuer));
        let request = request_digest("r", "o", "source-1", 10);
        assert_ne!(request, request_digest("r", "o", "source-1", 11));
        assert_ne!(request, request_digest("other", "o", "source-1", 10));
        assert_ne!(request, request_digest("r", "o", "source-2", 10));
        assert_ne!(
            sha256_domain(b"litecr.match.idempotency-key.v1", &[b"first-key"]),
            sha256_domain(b"litecr.match.idempotency-key.v1", &[b"second-key"])
        );
    }

    #[tokio::test]
    async fn evidence_read_checks_permission_and_record_scope_and_hides_out_of_scope_runs() {
        let calls = Arc::new(AtomicUsize::new(0));
        let service = MatchingService::new(
            Box::new(EvidenceStore {
                calls: calls.clone(),
                result: result(),
            }),
            100,
        )
        .unwrap();

        let returned = service
            .read_evidence(
                Some(&reviewer_context(None)),
                "r",
                "o",
                "00000000-0000-0000-0000-000000000001",
            )
            .await
            .expect("reviewer reads persisted snapshot");
        assert_eq!(returned, result());
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        let limited = reviewer_context(Some(BTreeSet::from(["other-record".into()])));
        assert!(matches!(
            service
                .read_evidence(
                    Some(&limited),
                    "r",
                    "o",
                    "00000000-0000-0000-0000-000000000001",
                )
                .await,
            Err(MatchingError::Store(MatchingStoreError::EvidenceNotFound))
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 2);

        assert!(matches!(
            service
                .read_evidence(
                    Some(&reviewer_context(None)),
                    "r",
                    "other-org",
                    "00000000-0000-0000-0000-000000000001",
                )
                .await,
            Err(MatchingError::Store(MatchingStoreError::EvidenceNotFound))
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn evidence_read_rejects_malformed_uuid_before_store_access() {
        let calls = Arc::new(AtomicUsize::new(0));
        let service = MatchingService::new(
            Box::new(EvidenceStore {
                calls: calls.clone(),
                result: result(),
            }),
            100,
        )
        .unwrap();
        assert!(matches!(
            service
                .read_evidence(Some(&reviewer_context(None)), "r", "o", "not-a-uuid")
                .await,
            Err(MatchingError::InvalidRequest)
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
}
