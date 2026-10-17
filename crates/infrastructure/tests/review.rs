use async_trait::async_trait;
use litecr_application::{
    access::{
        AccessService, AccessServiceConfig, AccessSessionRepository, ActorKind, ActorRef,
        OidcAuthorization, OidcFlow, OidcIdentityClaims, OidcProvider, OidcProviderError,
    },
    registry::RegistryStore,
    review::{ReviewActor, ReviewDetailAccess, ReviewQueueFilter, ReviewService, ReviewStore},
};
use litecr_domain::{
    registry::{SourceAssertion, SourceRecordCommand},
    review::{OpenReviewCase, ReviewDecisionCommand, ReviewDecisionKind},
};
use litecr_infrastructure::{
    access::{
        PostgresAccessAuthenticator, PostgresAccessDirectory, PostgresAccessSecurityAudit,
        PostgresAccessSessionRepository, PostgresOidcFlowStore,
    },
    connect, migrate,
    registry::PostgresRegistryStore,
    review::PostgresReviewStore,
};
use std::sync::Arc;

const SYNTHETIC_ISSUER: &str = "https://review-idp.example.test";

fn suffix() -> String {
    format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    )
}

fn all_records_access() -> ReviewDetailAccess {
    ReviewDetailAccess {
        actor_id: "synthetic-review-reader".into(),
        actor_issuer_url: Some("https://review-idp.example.test".into()),
        assigned_case_allowed: false,
        all_records: true,
        record_ids: Vec::new(),
    }
}

#[tokio::test]
async fn postgres_review_serializes_decisions_preserves_redirects_and_blocks_nonmatch_merge() {
    let url = std::env::var("LITECR_TEST_DATABASE_URL")
        .expect("set LITECR_TEST_DATABASE_URL to isolated synthetic database");
    let key = suffix();
    let task = tokio::spawn(scenario(url.clone(), key.clone())).await;
    cleanup(&url, &key).await;
    assert!(task.is_ok(), "review scenario task panicked: {task:?}");
    task.unwrap().expect("review PostgreSQL scenario");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn postgres_review_competing_decisions_have_one_winner_and_preserve_identity_invariant() {
    let url = std::env::var("LITECR_TEST_DATABASE_URL")
        .expect("set LITECR_TEST_DATABASE_URL to isolated synthetic database");
    let key = suffix();
    let task = tokio::spawn(competing_decisions_scenario(
        url.clone(),
        key.clone(),
        ReviewDecisionKind::Merge,
        ReviewDecisionKind::ConfirmedNonMatch,
    ))
    .await;
    cleanup(&url, &key).await;
    assert!(
        task.is_ok(),
        "concurrent review scenario task panicked: {task:?}"
    );
    task.unwrap()
        .expect("concurrent review PostgreSQL scenario");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn postgres_review_merge_and_link_contenders_have_one_winner() {
    let url = std::env::var("LITECR_TEST_DATABASE_URL")
        .expect("set LITECR_TEST_DATABASE_URL to isolated synthetic database");
    let key = suffix();
    let task = tokio::spawn(competing_decisions_scenario(
        url.clone(),
        key.clone(),
        ReviewDecisionKind::Merge,
        ReviewDecisionKind::Link,
    ))
    .await;
    cleanup(&url, &key).await;
    assert!(task.is_ok(), "merge/link scenario task panicked: {task:?}");
    task.unwrap().expect("merge/link PostgreSQL scenario");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn postgres_review_link_and_nonmatch_contenders_preserve_identity_invariant() {
    let url = std::env::var("LITECR_TEST_DATABASE_URL")
        .expect("set LITECR_TEST_DATABASE_URL to isolated synthetic database");
    let key = suffix();
    let task = tokio::spawn(competing_decisions_scenario(
        url.clone(),
        key.clone(),
        ReviewDecisionKind::Link,
        ReviewDecisionKind::ConfirmedNonMatch,
    ))
    .await;
    cleanup(&url, &key).await;
    assert!(
        task.is_ok(),
        "link/non-match scenario task panicked: {task:?}"
    );
    task.unwrap().expect("link/non-match PostgreSQL scenario");
}

#[tokio::test]
async fn postgres_review_queue_keyset_pages_over_fifty_cases_without_duplicates_or_skips() {
    let url = std::env::var("LITECR_TEST_DATABASE_URL")
        .expect("set LITECR_TEST_DATABASE_URL to isolated synthetic database");
    let key = suffix();
    let task = tokio::spawn(queue_pagination_scenario(url.clone(), key.clone())).await;
    cleanup(&url, &key).await;
    assert!(task.is_ok(), "review pagination task panicked: {task:?}");
    task.unwrap().expect("review keyset pagination scenario");
}

#[tokio::test]
async fn postgres_review_detail_enforces_assigned_or_both_record_grants_through_access_service() {
    let url = std::env::var("LITECR_TEST_DATABASE_URL")
        .expect("set LITECR_TEST_DATABASE_URL to isolated synthetic database");
    let key = suffix();
    let task = tokio::spawn(review_detail_access_scenario(url.clone(), key.clone())).await;
    cleanup(&url, &key).await;
    assert!(task.is_ok(), "review detail access task panicked: {task:?}");
    task.unwrap()
        .expect("review detail authorization over PostgreSQL");
}

async fn review_detail_access_scenario(url: String, key: String) -> Result<(), String> {
    let pool = connect(&url, 5)
        .await
        .map_err(|_| "synthetic review database unavailable")?;
    let database: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(&pool)
        .await
        .map_err(|_| "database name queried")?;
    ensure(
        database == "litecr_test",
        "database guard permits only litecr_test",
    )?;
    migrate(&pool)
        .await
        .map_err(|_| "synthetic review migrations unavailable")?;

    let registry = format!("review-reg-{key}");
    let org = format!("review-org-{key}");
    let assigner_subject = format!("synthetic-review-assigner-{key}");
    let assigned_subject = format!("synthetic-review-assigned-{key}");
    let missing_subject = format!("synthetic-review-missing-{key}");
    let inactive_subject = format!("synthetic-review-inactive-{key}");
    let wrong_org_subject = format!("synthetic-review-wrong-org-{key}");
    let integration_subject = format!("synthetic-review-integration-only-{key}");
    let one_record_subject = format!("synthetic-review-one-record-{key}");
    let both_records_subject = format!("synthetic-review-both-records-{key}");
    let assigner = provision_review_access(
        &pool,
        &key,
        &registry,
        &org,
        &assigner_subject,
        "review:decide",
        None,
    )
    .await?;
    let assigned = provision_review_access(
        &pool,
        &format!("{key}-assignee"),
        &registry,
        &org,
        &assigned_subject,
        "review:decide",
        None,
    )
    .await?;

    let registry_store = PostgresRegistryStore::new(pool.clone());
    let registry_actor = litecr_application::registry::RegistryActor {
        kind: ActorKind::Human,
        id: format!("synthetic-review-detail-registrar-{key}"),
        issuer_url: Some(SYNTHETIC_ISSUER.into()),
        request_id: format!("review-detail-ingest-{key}"),
        correlation_id: Some(format!("review-detail-corr-{key}")),
    };
    let ingest = |name: &str| SourceRecordCommand {
        registry_id: registry.clone(),
        organization_id: org.clone(),
        source_system_id: format!("review-detail-source-{key}"),
        source_record_key: name.into(),
        source_version: Some("v1".into()),
        assertions: vec![SourceAssertion {
            field_path: "name.original".into(),
            original_value: format!("Synthetic detail case {name} {key}"),
            language_tag: Some("en".into()),
            script: Some("Latn".into()),
        }],
        identifiers: vec![],
    };
    let query = registry_store
        .ingest(
            &registry_actor,
            &format!("review-detail-{key}-query"),
            &ingest("query"),
        )
        .await
        .map_err(|_| "synthetic query record ingested")?;
    let candidate = registry_store
        .ingest(
            &registry_actor,
            &format!("review-detail-{key}-candidate"),
            &ingest("candidate"),
        )
        .await
        .map_err(|_| "synthetic candidate record ingested")?;

    let store = PostgresReviewStore::new(pool.clone());
    let case = store
        .open_case(
            &ReviewActor {
                id: registry_actor.id.clone(),
                issuer_url: registry_actor.issuer_url.clone(),
                request_id: registry_actor.request_id.clone(),
                correlation_id: registry_actor.correlation_id.clone(),
            },
            &OpenReviewCase {
                registry_id: registry.clone(),
                organization_id: org.clone(),
                query_source_record_id: query.source_record_id.clone(),
                candidate_source_record_id: candidate.source_record_id.clone(),
                evidence_run_id: None,
                expected_query_receive_version: 1,
                expected_candidate_receive_version: 1,
            },
        )
        .await
        .map_err(|_| "synthetic review case opened")?;
    let review = ReviewService::with_access_directory(
        Box::new(store),
        Arc::new(PostgresAccessDirectory::new(pool.clone())),
    );

    provision_ineligible_review_targets(
        &pool,
        &registry,
        &org,
        &inactive_subject,
        &wrong_org_subject,
        &integration_subject,
    )
    .await?;
    for rejected_subject in [
        missing_subject.as_str(),
        inactive_subject.as_str(),
        wrong_org_subject.as_str(),
        integration_subject.as_str(),
    ] {
        ensure(
            matches!(
                review
                    .assign(
                        Some(&assigner),
                        &case.case_id,
                        &registry,
                        &org,
                        case.case_version,
                        rejected_subject,
                    )
                    .await,
                Err(litecr_application::review::ReviewError::AccessDenied(
                    litecr_application::access::AccessDenied::PermissionMissing
                ))
            ),
            "assignment rejects unprovisioned, inactive, wrong-scope or non-human targets",
        )?;
    }
    let assigned_case = review
        .assign(
            Some(&assigner),
            &case.case_id,
            &registry,
            &org,
            case.case_version,
            &assigned_subject,
        )
        .await
        .map_err(|error| format!("reviewer assigned own case: {error:?}"))?;
    let decision = review
        .decide(
            Some(&assigned),
            &registry,
            &org,
            ReviewDecisionCommand {
                case_id: case.case_id.clone(),
                expected_case_version: assigned_case.case_version,
                expected_query_receive_version: 1,
                expected_candidate_receive_version: 1,
                expected_query_person_version: 1,
                expected_candidate_person_version: 1,
                kind: ReviewDecisionKind::ConfirmedNonMatch,
                reason_code: "synthetic_detail_access".into(),
                reason: "Synthetic review detail access regression fixture.".into(),
                idempotency_key: format!("review-detail-decision-{key}"),
                merge_id: None,
            },
        )
        .await
        .map_err(|error| format!("assigned reviewer decided case: {error:?}"))?;
    ensure(
        decision.outcome == "accepted",
        "synthetic decision accepted",
    )?;

    let assigned_detail = review
        .detail(Some(&assigned), &case.case_id, &registry, &org)
        .await
        .map_err(|error| format!("assigned reviewer reads detail without ReviewRead: {error:?}"))?;
    ensure(
        assigned_detail.history.len() == 1,
        "assigned reviewer can read persisted decision history without ReviewRead",
    )?;

    let one_record = provision_review_access(
        &pool,
        &format!("{key}-one"),
        &registry,
        &org,
        &one_record_subject,
        "review:read",
        Some(vec![query.source_record_id.clone()]),
    )
    .await?;
    ensure(
        matches!(
            review
                .detail(Some(&one_record), &case.case_id, &registry, &org)
                .await,
            Err(litecr_application::review::ReviewError::Store(
                litecr_application::review::ReviewStoreError::NotFound
            ))
        ),
        "one source record grant cannot reveal the case or history",
    )?;

    let both_records = provision_review_access(
        &pool,
        &format!("{key}-both"),
        &registry,
        &org,
        &both_records_subject,
        "review:read",
        Some(vec![query.source_record_id, candidate.source_record_id]),
    )
    .await?;
    let both_detail = review
        .detail(Some(&both_records), &case.case_id, &registry, &org)
        .await
        .map_err(|error| format!("grants for both sources read detail: {error:?}"))?;
    ensure(
        both_detail.history.len() == 1,
        "grants for both source records reveal the persisted history",
    )?;
    Ok(())
}

async fn provision_review_access(
    pool: &sqlx::PgPool,
    key: &str,
    registry: &str,
    organization: &str,
    subject: &str,
    permission: &str,
    record_ids: Option<Vec<String>>,
) -> Result<litecr_application::access::AccessContext, String> {
    sqlx::query("INSERT INTO litecr.access_identity (actor_kind,issuer_url,external_subject) VALUES ('human',$1,$2)")
        .bind(SYNTHETIC_ISSUER)
        .bind(subject)
        .execute(pool)
        .await
        .map_err(|_| "synthetic review identity inserted")?;
    sqlx::query("INSERT INTO litecr.access_grant (actor_kind,issuer_url,external_subject,registry_id,organization_id,record_ids,permission_name) VALUES ('human',$1,$2,$3,$4,$5,$6)")
        .bind(SYNTHETIC_ISSUER)
        .bind(subject)
        .bind(registry)
        .bind(organization)
        .bind(record_ids)
        .bind(permission)
        .execute(pool)
        .await
        .map_err(|_| "synthetic review permission inserted")?;
    let directory = PostgresAccessDirectory::new(pool.clone());
    let sessions = PostgresAccessSessionRepository::new(pool.clone(), directory.clone());
    let actor = ActorRef {
        kind: ActorKind::Human,
        id: subject.into(),
        issuer_url: Some(SYNTHETIC_ISSUER.into()),
    };
    let session = sessions
        .create(
            &actor,
            Some(&format!("synthetic-review-session-{key}")),
            600,
            &format!("review-session-{key}"),
        )
        .await
        .map_err(|_| "synthetic review session created")?;
    let service = AccessService::new(
        Arc::new(UnusedOidcProvider),
        Arc::new(PostgresOidcFlowStore::new(pool.clone())),
        Arc::new(sessions.clone()),
        Arc::new(directory),
        Arc::new(PostgresAccessAuthenticator::new(sessions)),
        Arc::new(PostgresAccessSecurityAudit::new(pool.clone())),
        AccessServiceConfig {
            login_flow_lifetime_seconds: 300,
            session_lifetime_seconds: 600,
        },
    )
    .ok_or("synthetic AccessService config valid")?;
    service
        .authenticate(
            &session.opaque_token,
            Some(&session.csrf_token),
            true,
            &format!("review-auth-{key}"),
        )
        .await
        .map_err(|_| "AccessService authenticates synthetic scoped grant".into())
}

async fn provision_ineligible_review_targets(
    pool: &sqlx::PgPool,
    registry: &str,
    organization: &str,
    inactive_subject: &str,
    wrong_org_subject: &str,
    integration_subject: &str,
) -> Result<(), String> {
    sqlx::query("INSERT INTO litecr.access_identity (actor_kind,issuer_url,external_subject,is_active) VALUES ('human',$1,$2,FALSE),('human',$1,$3,TRUE),('integration_client','',$4,TRUE)")
        .bind(SYNTHETIC_ISSUER)
        .bind(inactive_subject)
        .bind(wrong_org_subject)
        .bind(integration_subject)
        .execute(pool)
        .await
        .map_err(|_| "ineligible synthetic review identities inserted")?;
    sqlx::query("INSERT INTO litecr.access_grant (actor_kind,issuer_url,external_subject,registry_id,organization_id,permission_name) VALUES ('human',$1,$2,$3,$4,'review:decide'),('human',$1,$5,$3,$6,'review:decide'),('integration_client','',$7,$3,$4,'review:decide')")
        .bind(SYNTHETIC_ISSUER)
        .bind(inactive_subject)
        .bind(registry)
        .bind(organization)
        .bind(wrong_org_subject)
        .bind(format!("{organization}-different"))
        .bind(integration_subject)
        .execute(pool)
        .await
        .map_err(|_| "ineligible synthetic review grants inserted")?;
    Ok(())
}

struct UnusedOidcProvider;

#[async_trait]
impl OidcProvider for UnusedOidcProvider {
    async fn begin_authorization(&self) -> Result<OidcAuthorization, OidcProviderError> {
        Err(OidcProviderError::Unavailable)
    }

    async fn exchange_code(
        &self,
        _code: &str,
        _flow: &OidcFlow,
    ) -> Result<OidcIdentityClaims, OidcProviderError> {
        Err(OidcProviderError::Unavailable)
    }
}

async fn queue_pagination_scenario(url: String, key: String) -> Result<(), String> {
    let pool = connect(&url, 5)
        .await
        .map_err(|_| "synthetic review database unavailable")?;
    migrate(&pool)
        .await
        .map_err(|_| "synthetic review migrations unavailable")?;
    let registry = format!("review-reg-{key}");
    let org = format!("review-org-{key}");
    let registrar = litecr_application::registry::RegistryActor {
        kind: ActorKind::Human,
        id: format!("synthetic-page-registrar-{key}"),
        issuer_url: Some("https://review-idp.example.test".into()),
        request_id: format!("review-page-ingest-{key}"),
        correlation_id: Some(format!("review-page-corr-{key}")),
    };
    let reviewer = ReviewActor {
        id: format!("synthetic-page-reviewer-{key}"),
        issuer_url: Some("https://review-idp.example.test".into()),
        request_id: format!("review-page-request-{key}"),
        correlation_id: Some(format!("review-page-corr-{key}")),
    };
    let registry_store = PostgresRegistryStore::new(pool.clone());
    let review_store = PostgresReviewStore::new(pool.clone());
    let ingest = |label: String| SourceRecordCommand {
        registry_id: registry.clone(),
        organization_id: org.clone(),
        source_system_id: format!("page-source-{key}"),
        source_record_key: label.clone(),
        source_version: Some("v1".into()),
        assertions: vec![SourceAssertion {
            field_path: "name.original".into(),
            original_value: format!("Synthetic page fixture {label}"),
            language_tag: Some("en".into()),
            script: Some("Latn".into()),
        }],
        identifiers: vec![],
    };
    let query = registry_store
        .ingest(
            &registrar,
            &format!("page-ingest-{key}-query"),
            &ingest("query".into()),
        )
        .await
        .map_err(|_| "synthetic query source record failed")?;
    let mut opened_case_ids = Vec::new();
    for index in 0..55 {
        let label = format!("candidate-{index:02}");
        let candidate = registry_store
            .ingest(
                &registrar,
                &format!("page-ingest-{key}-{label}"),
                &ingest(label),
            )
            .await
            .map_err(|_| "synthetic candidate source record failed")?;
        let case = review_store
            .open_case(
                &reviewer,
                &OpenReviewCase {
                    registry_id: registry.clone(),
                    organization_id: org.clone(),
                    query_source_record_id: query.source_record_id.clone(),
                    candidate_source_record_id: candidate.source_record_id,
                    evidence_run_id: None,
                    expected_query_receive_version: 1,
                    expected_candidate_receive_version: 1,
                },
            )
            .await
            .map_err(|_| "synthetic review case failed")?;
        opened_case_ids.push(case.case_id);
    }

    let initial_decision_case = review_store
        .detail(&opened_case_ids[0], &registry, &org, &all_records_access())
        .await
        .map_err(|_| "initial resolved-case detail failed")?
        .case;
    let one_source_access = ReviewDetailAccess {
        actor_id: reviewer.id.clone(),
        actor_issuer_url: reviewer.issuer_url.clone(),
        assigned_case_allowed: false,
        all_records: false,
        record_ids: vec![initial_decision_case.query_source_record_id.clone()],
    };
    ensure(
        matches!(
            review_store
                .detail(
                    &initial_decision_case.case_id,
                    &registry,
                    &org,
                    &one_source_access,
                )
                .await,
            Err(litecr_application::review::ReviewStoreError::NotFound)
        ),
        "grant for only one case source record cannot read detail or history",
    )?;
    let both_sources_access = ReviewDetailAccess {
        actor_id: reviewer.id.clone(),
        actor_issuer_url: reviewer.issuer_url.clone(),
        assigned_case_allowed: false,
        all_records: false,
        record_ids: vec![
            initial_decision_case.query_source_record_id.clone(),
            initial_decision_case.candidate_source_record_id.clone(),
        ],
    };
    ensure(
        review_store
            .detail(
                &initial_decision_case.case_id,
                &registry,
                &org,
                &both_sources_access,
            )
            .await
            .is_ok(),
        "grants for both source records permit case detail and history",
    )?;
    resolve_pagination_case(
        &review_store,
        &reviewer,
        &initial_decision_case,
        &registry,
        &org,
        &key,
    )
    .await?;

    let first = review_store
        .list_cases(&registry, &org, ReviewQueueFilter::Active, 50, None)
        .await
        .map_err(|_| "first review queue page failed")?;
    ensure(
        first.cases.len() == 50,
        "first page returns the requested bounded count",
    )?;
    ensure(first.has_more, "first page honestly signals more cases")?;
    let cursor = first
        .next_cursor
        .clone()
        .ok_or("first page omitted its continuation cursor")?;
    let expected_all = sqlx::query_scalar::<_, String>(
        "SELECT case_id::text FROM litecr.review_case WHERE registry_id=$1 AND organization_id=$2 ORDER BY created_at DESC,case_id DESC",
    )
    .bind(&registry)
    .bind(&org)
    .fetch_all(&pool)
    .await
    .map_err(|_| "expected synthetic queue order query failed")?;
    ensure(
        expected_all.len() == opened_case_ids.len(),
        "all synthetic cases appear in the reference order",
    )?;
    ensure(
        first.cases[0].case_id == expected_all[0],
        "newest active case is visible on the first page",
    )?;
    let expected_active = expected_all
        .iter()
        .filter(|case_id| case_id.as_str() != opened_case_ids[0])
        .cloned()
        .collect::<Vec<_>>();
    let first_resolved = review_store
        .list_cases(&registry, &org, ReviewQueueFilter::Resolved, 50, None)
        .await
        .map_err(|_| "resolved queue request failed")?;
    ensure(
        first_resolved.cases.len() == 1 && !first_resolved.has_more,
        "resolved queue keeps decided cases reachable",
    )?;

    // Mutating state after page one must not move a case across keyset boundaries.
    let assigned = &first.cases[10];
    review_store
        .assign(
            &reviewer,
            &assigned.case_id,
            &registry,
            &org,
            assigned.case_version,
            &reviewer.id,
        )
        .await
        .map_err(|_| "synthetic case assignment during paging failed")?;
    let decision_after_first_page = review_store
        .detail(&assigned.case_id, &registry, &org, &all_records_access())
        .await
        .map_err(|_| "assigned case detail failed")?
        .case;
    ensure(
        review_store
            .detail(
                &assigned.case_id,
                &registry,
                &org,
                &ReviewDetailAccess {
                    actor_id: reviewer.id.clone(),
                    actor_issuer_url: reviewer.issuer_url.clone(),
                    assigned_case_allowed: true,
                    all_records: false,
                    record_ids: Vec::new(),
                },
            )
            .await
            .is_ok(),
        "assigned actor can access case detail without broad record grants",
    )?;
    ensure(
        matches!(
            review_store
                .detail(
                    &assigned.case_id,
                    &registry,
                    &org,
                    &ReviewDetailAccess {
                        // Same external subject from another issuer must not
                        // inherit this assignment.
                        actor_id: reviewer.id.clone(),
                        actor_issuer_url: Some("https://other-idp.example.test".into()),
                        assigned_case_allowed: true,
                        all_records: false,
                        record_ids: Vec::new(),
                    },
                )
                .await,
            Err(litecr_application::review::ReviewStoreError::NotFound)
        ),
        "same subject from a different issuer cannot use the case assignment",
    )?;
    ensure(
        matches!(
            review_store
                .detail(
                    &assigned.case_id,
                    &registry,
                    &org,
                    &ReviewDetailAccess {
                        // Same subject and issuer as the assigned Human,
                        // but integration identities cannot use assignment.
                        actor_id: reviewer.id.clone(),
                        actor_issuer_url: reviewer.issuer_url.clone(),
                        assigned_case_allowed: false,
                        all_records: false,
                        record_ids: Vec::new(),
                    },
                )
                .await,
            Err(litecr_application::review::ReviewStoreError::NotFound)
        ),
        "same issuer and subject with non-human actor kind cannot use assignment",
    )?;
    resolve_pagination_case(
        &review_store,
        &reviewer,
        &decision_after_first_page,
        &registry,
        &org,
        &key,
    )
    .await?;

    let mut observed = first
        .cases
        .iter()
        .map(|item| item.case_id.clone())
        .collect::<Vec<_>>();
    let second = review_store
        .list_cases(
            &registry,
            &org,
            ReviewQueueFilter::Active,
            50,
            Some(&cursor),
        )
        .await
        .map_err(|_| "second review queue page failed")?;
    ensure(
        second.cases.len() == 4,
        "second page returns all remaining cases",
    )?;
    ensure(
        !second.has_more,
        "last page reports that no more cases remain",
    )?;
    ensure(
        second.next_cursor.is_none(),
        "last page does not advertise a false continuation",
    )?;
    observed.extend(second.cases.into_iter().map(|item| item.case_id));
    observed.retain(|case_id| case_id != &assigned.case_id);
    let expected_active = expected_active
        .into_iter()
        .filter(|case_id| case_id != &assigned.case_id)
        .collect::<Vec<_>>();
    ensure(
        observed == expected_active,
        "active keyset pages have no skips after a case moves to resolved",
    )?;
    ensure(
        observed
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            == observed.len(),
        "keyset pages contain no duplicate cases",
    )?;
    let resolved_after = review_store
        .list_cases(&registry, &org, ReviewQueueFilter::Resolved, 50, None)
        .await
        .map_err(|_| "resolved queue refresh failed")?;
    ensure(
        resolved_after.cases.len() == 2,
        "cases decided while paging remain reachable in resolved queue",
    )?;
    let all_after = review_store
        .list_cases(&registry, &org, ReviewQueueFilter::All, 100, None)
        .await
        .map_err(|_| "all-cases compatibility request failed")?;
    ensure(
        all_after.cases.len() == 55,
        "legacy all-cases filter remains complete",
    )?;
    Ok(())
}

async fn resolve_pagination_case(
    store: &PostgresReviewStore,
    reviewer: &ReviewActor,
    case: &litecr_application::review::ReviewCaseView,
    registry: &str,
    organization: &str,
    key: &str,
) -> Result<(), String> {
    store
        .decide(
            reviewer,
            registry,
            organization,
            &ReviewDecisionCommand {
                case_id: case.case_id.clone(),
                expected_case_version: case.case_version,
                expected_query_receive_version: case.query_receive_version,
                expected_candidate_receive_version: case.candidate_receive_version,
                expected_query_person_version: case.query_person_version,
                expected_candidate_person_version: case.candidate_person_version,
                kind: ReviewDecisionKind::ConfirmedNonMatch,
                reason_code: "synthetic-pagination-test".into(),
                reason: "Synthetic distinct sources for pagination mechanics.".into(),
                idempotency_key: format!("page-decision-{key}-{}", case.case_id),
                merge_id: None,
            },
        )
        .await
        .map_err(|_| "synthetic review decision failed")?;
    Ok(())
}

async fn competing_decisions_scenario(
    url: String,
    key: String,
    first_kind: ReviewDecisionKind,
    contender_kind: ReviewDecisionKind,
) -> Result<(), String> {
    let pool = connect(&url, 5)
        .await
        .map_err(|_| "synthetic review DB unavailable")?;
    migrate(&pool)
        .await
        .map_err(|_| "synthetic review migrations unavailable")?;
    let registry = format!("review-reg-{key}");
    let org = format!("review-org-{key}");
    let registrar = litecr_application::registry::RegistryActor {
        kind: ActorKind::Human,
        id: format!("synthetic-race-reg-{key}"),
        issuer_url: Some("https://review-idp.example.test".into()),
        request_id: format!("review-race-ingest-{key}"),
        correlation_id: Some(format!("review-race-corr-{key}")),
    };
    let records = PostgresRegistryStore::new(pool.clone());
    let mut source_ids = Vec::new();
    let mut public_ids = Vec::new();
    for label in ["left", "right"] {
        let receipt = records
            .ingest(
                &registrar,
                &format!("race-ingest-key-{key}-{label}"),
                &SourceRecordCommand {
                    registry_id: registry.clone(),
                    organization_id: org.clone(),
                    source_system_id: format!("race-src-{key}-{label}"),
                    source_record_key: format!("race-key-{key}-{label}"),
                    source_version: Some("v1".into()),
                    assertions: vec![SourceAssertion {
                        field_path: "name.original".into(),
                        original_value: format!("Synthetic race {label} {key}"),
                        language_tag: Some("en".into()),
                        script: Some("Latn".into()),
                    }],
                    identifiers: vec![],
                },
            )
            .await
            .map_err(|_| "synthetic race registry seed failed")?;
        source_ids.push(receipt.source_record_id);
        public_ids.push(receipt.person_public_id);
    }

    let review = PostgresReviewStore::new(pool.clone());
    let case = review
        .open_case(
            &ReviewActor {
                id: format!("synthetic-race-opener-{key}"),
                issuer_url: Some("https://review-idp.example.test".into()),
                request_id: format!("review-race-open-{key}"),
                correlation_id: None,
            },
            &OpenReviewCase {
                registry_id: registry.clone(),
                organization_id: org.clone(),
                query_source_record_id: source_ids[0].clone(),
                candidate_source_record_id: source_ids[1].clone(),
                evidence_run_id: None,
                expected_query_receive_version: 1,
                expected_candidate_receive_version: 1,
            },
        )
        .await
        .map_err(|_| "synthetic race review case create failed")?;

    let first_name = decision_label(&first_kind)?;
    let first_actor = ReviewActor {
        id: format!("synthetic-race-{first_name}-{key}"),
        issuer_url: Some("https://review-idp.example.test".into()),
        request_id: format!("review-race-{first_name}-{key}"),
        correlation_id: Some(format!("review-race-corr-{key}")),
    };
    let contender_name = decision_label(&contender_kind)?;
    let contender_actor = ReviewActor {
        id: format!("synthetic-race-{contender_name}-{key}"),
        issuer_url: Some("https://review-idp.example.test".into()),
        request_id: format!("review-race-{contender_name}-{key}"),
        correlation_id: Some(format!("review-race-corr-{key}")),
    };
    let first = ReviewDecisionCommand {
        case_id: case.case_id.clone(),
        expected_case_version: case.case_version,
        expected_query_receive_version: 1,
        expected_candidate_receive_version: 1,
        expected_query_person_version: 1,
        expected_candidate_person_version: 1,
        kind: first_kind,
        reason_code: format!("race_{first_name}_candidate"),
        reason: "Synthetic concurrent competing decision".into(),
        idempotency_key: format!("race-{first_name}-idem-{key}"),
        merge_id: None,
    };
    let contender = ReviewDecisionCommand {
        case_id: case.case_id.clone(),
        expected_case_version: case.case_version,
        expected_query_receive_version: 1,
        expected_candidate_receive_version: 1,
        expected_query_person_version: 1,
        expected_candidate_person_version: 1,
        kind: contender_kind,
        reason_code: format!("race_{contender_name}_candidate"),
        reason: "Synthetic concurrent competing decision".into(),
        idempotency_key: format!("race-{contender_name}-idem-{key}"),
        merge_id: None,
    };

    // Release both real PostgreSQL decisions together after all setup is complete.
    let start = std::sync::Arc::new(tokio::sync::Barrier::new(3));
    let first_store = review.clone();
    let first_registry = registry.clone();
    let first_org = org.clone();
    let first_start = start.clone();
    let first_task = tokio::spawn(async move {
        first_start.wait().await;
        first_store
            .decide(&first_actor, &first_registry, &first_org, &first)
            .await
    });
    let contender_store = review.clone();
    let contender_registry = registry.clone();
    let contender_org = org.clone();
    let contender_start = start.clone();
    let contender_task = tokio::spawn(async move {
        contender_start.wait().await;
        contender_store
            .decide(
                &contender_actor,
                &contender_registry,
                &contender_org,
                &contender,
            )
            .await
    });
    start.wait().await;
    let first_result = first_task
        .await
        .map_err(|_| "first decision contender task panicked")?;
    let contender_result = contender_task
        .await
        .map_err(|_| "competing decision task panicked")?;

    let (winning_decision, winning_actor, winning_result, stale_loser_conflicted, loser_error) =
        match (first_result, contender_result) {
            (Ok(result), Err(error)) => (
                first_name,
                format!("synthetic-race-{first_name}-{key}"),
                result,
                error == litecr_application::review::ReviewStoreError::Conflict,
                format!("{error:?}"),
            ),
            (Err(error), Ok(result)) => (
                contender_name,
                format!("synthetic-race-{contender_name}-{key}"),
                result,
                error == litecr_application::review::ReviewStoreError::Conflict,
                format!("{error:?}"),
            ),
            (first, contender) => {
                return Err(format!(
                    "exactly one decision must win; first={first:?}, contender={contender:?}"
                ));
            }
        };
    ensure(
        winning_result.outcome == "accepted" && !winning_result.replayed,
        "the winning concurrent decision must be newly accepted",
    )?;
    ensure(
        winning_result.case.state == "decided" && winning_result.case.case_version == 2,
        "one accepted decision advances the case exactly once",
    )?;

    let history = review
        .detail(&case.case_id, &registry, &org, &all_records_access())
        .await
        .map_err(|error| format!("concurrent decision history read failed: {error:?}"))?;
    ensure(
        history.history.len() == 1,
        "the concurrent race leaves exactly one committed history entry",
    )?;
    let winner = &history.history[0];
    ensure(
        winner.decision == winning_decision
            && winner.outcome == "accepted"
            && winner.actor_id == winning_actor
            && winner.reason_code == format!("race_{winning_decision}_candidate"),
        "history must identify the accepted decision, its actor, and its reason",
    )?;

    if winning_decision == "merge" {
        let left = records
            .person(&registry, &org, &public_ids[0])
            .await
            .map_err(|_| "race left identity lookup failed")?
            .ok_or("race left identity missing")?;
        let right = records
            .person(&registry, &org, &public_ids[1])
            .await
            .map_err(|_| "race right identity lookup failed")?
            .ok_or("race right identity missing")?;
        ensure(
            left.public_id == public_ids[1] && right.public_id == public_ids[1],
            "merge winner redirects both source identities to the survivor",
        )?;
        ensure(
            winner.merge_id.is_some() && winning_result.merge_id == winner.merge_id,
            "merge winner history and accepted result reference the same merge",
        )?;
        ensure(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM litecr.confirmed_non_match WHERE registry_id=$1 AND organization_id=$2")
                .bind(&registry)
                .bind(&org)
                .fetch_one(&pool)
                .await
                .map_err(|_| "merge winner non-match invariant query failed")? == 0,
            "merge winner must not create a confirmed non-match",
        )?;
    } else if winning_decision == "confirmed_non_match" {
        let left = records
            .person(&registry, &org, &public_ids[0])
            .await
            .map_err(|_| "race left identity lookup failed")?
            .ok_or("race left identity missing")?;
        let right = records
            .person(&registry, &org, &public_ids[1])
            .await
            .map_err(|_| "race right identity lookup failed")?
            .ok_or("race right identity missing")?;
        ensure(
            left.public_id == public_ids[0] && right.public_id == public_ids[1],
            "non-match winner preserves both independent identities",
        )?;
        ensure(
            winner.merge_id.is_none() && winning_result.merge_id.is_none(),
            "confirmed non-match winner must not reference a merge",
        )?;
        ensure(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM litecr.confirmed_non_match WHERE registry_id=$1 AND organization_id=$2")
                .bind(&registry)
                .bind(&org)
                .fetch_one(&pool)
                .await
                .map_err(|_| "non-match winner invariant query failed")? == 1,
            "non-match winner persists exactly one confirmed non-match",
        )?;
    } else {
        let link_count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM litecr.identity_link WHERE registry_id=$1 AND organization_id=$2",
        )
        .bind(&registry)
        .bind(&org)
        .fetch_one(&pool)
        .await
        .map_err(|_| "link winner invariant query failed")?;
        let merge_count: i64 = sqlx::query_scalar("SELECT count(*) FROM litecr.identity_merge WHERE registry_id=$1 AND organization_id=$2")
            .bind(&registry)
            .bind(&org)
            .fetch_one(&pool)
            .await
            .map_err(|_| "link winner merge invariant query failed")?;
        let non_match_count: i64 = sqlx::query_scalar("SELECT count(*) FROM litecr.confirmed_non_match WHERE registry_id=$1 AND organization_id=$2")
            .bind(&registry)
            .bind(&org)
            .fetch_one(&pool)
            .await
            .map_err(|_| "link winner non-match invariant query failed")?;
        let linked_source_count: i64 = sqlx::query_scalar("SELECT count(*) FROM litecr.source_record WHERE registry_id=$1 AND organization_id=$2 AND person_id=(SELECT target_person_id FROM litecr.identity_link WHERE registry_id=$1 AND organization_id=$2 LIMIT 1)")
            .bind(&registry)
            .bind(&org)
            .fetch_one(&pool)
            .await
            .map_err(|_| "link winner source assignment query failed")?;
        ensure(
            link_count == 1 && merge_count == 0,
            "link winner persists exactly one link and no merge",
        )?;
        ensure(
            non_match_count == 0,
            "link winner must not create a confirmed non-match",
        )?;
        ensure(
            linked_source_count == 2,
            "link winner assigns both synthetic sources to the linked target",
        )?;
        ensure(
            winner.merge_id.is_none() && winning_result.merge_id.is_none(),
            "link winner must not reference a merge",
        )?;
    }
    ensure(
        stale_loser_conflicted,
        &format!("stale competing decision must return Conflict; observed {loser_error}"),
    )?;
    Ok(())
}

async fn scenario(url: String, key: String) -> Result<(), String> {
    let pool = connect(&url, 5)
        .await
        .map_err(|_| "synthetic review DB unavailable")?;
    migrate(&pool)
        .await
        .map_err(|_| "synthetic review migrations unavailable")?;
    let registry = format!("review-reg-{key}");
    let org = format!("review-org-{key}");
    let registrar = litecr_application::registry::RegistryActor {
        kind: ActorKind::Human,
        id: format!("synthetic-reg-{key}"),
        issuer_url: Some("https://review-idp.example.test".into()),
        request_id: format!("review-ingest-{key}"),
        correlation_id: Some(format!("review-corr-{key}")),
    };
    let records = PostgresRegistryStore::new(pool.clone());
    let mut source_ids = Vec::new();
    let mut public_ids = Vec::new();
    for label in ["a", "b", "c", "withdrawn"] {
        let receipt = records
            .ingest(
                &registrar,
                &format!("ingest-key-{key}-{label}"),
                &SourceRecordCommand {
                    registry_id: registry.clone(),
                    organization_id: org.clone(),
                    source_system_id: format!("src-{key}-{label}"),
                    source_record_key: format!("key-{key}-{label}"),
                    source_version: Some("v1".into()),
                    assertions: vec![SourceAssertion {
                        field_path: "name.original".into(),
                        original_value: format!("Synthetic {label} {key}"),
                        language_tag: Some("en".into()),
                        script: Some("Latn".into()),
                    }],
                    identifiers: vec![],
                },
            )
            .await
            .map_err(|_| "synthetic registry seed failed")?;
        source_ids.push(receipt.source_record_id);
        public_ids.push(receipt.person_public_id);
    }
    let reviewer = ReviewActor {
        id: format!("synthetic-reviewer-{key}"),
        issuer_url: Some("https://review-idp.example.test".into()),
        request_id: format!("review-request-{key}"),
        correlation_id: Some(format!("review-corr-{key}")),
    };
    let review = PostgresReviewStore::new(pool.clone());
    let open = |a: &str, b: &str| OpenReviewCase {
        registry_id: registry.clone(),
        organization_id: org.clone(),
        query_source_record_id: a.into(),
        candidate_source_record_id: b.into(),
        evidence_run_id: None,
        expected_query_receive_version: 1,
        expected_candidate_receive_version: 1,
    };
    let case = review
        .open_case(&reviewer, &open(&source_ids[0], &source_ids[1]))
        .await
        .map_err(|_| "review case create failed")?;
    let withdrawn_case = review
        .open_case(&reviewer, &open(&source_ids[3], &source_ids[0]))
        .await
        .map_err(|_| "pre-withdrawal review case seed failed")?;
    let withdrawal_manifest = vec![8_u8; 32];
    sqlx::query("UPDATE litecr.source_record SET withdrawn_at=now(),withdrawn_by_manifest=$2 WHERE source_record_id=$1::uuid")
        .bind(&source_ids[3])
        .bind(&withdrawal_manifest)
        .execute(&pool)
        .await
        .map_err(|_| "synthetic source withdrawal marker written")?;
    sqlx::query("UPDATE litecr.source_record_version SET withdrawn_at=now(),withdrawn_by_manifest=$2 WHERE source_record_id=$1::uuid")
        .bind(&source_ids[3])
        .bind(&withdrawal_manifest)
        .execute(&pool)
        .await
        .map_err(|_| "synthetic version withdrawal marker written")?;
    sqlx::query("UPDATE litecr.source_assertion SET withdrawn_at=now(),withdrawn_by_manifest=$2 WHERE source_record_id=$1::uuid")
        .bind(&source_ids[3])
        .bind(&withdrawal_manifest)
        .execute(&pool)
        .await
        .map_err(|_| "synthetic assertion withdrawal marker written")?;
    let visible_cases = review
        .list_cases(&registry, &org, ReviewQueueFilter::All, 100, None)
        .await
        .map_err(|error| format!("review list with withdrawn source completes: {error:?}"))?;
    ensure(
        visible_cases.cases.iter().all(|item| {
            item.query_source_record_id != source_ids[3]
                && item.candidate_source_record_id != source_ids[3]
        }),
        "review queue hides cases linked to withdrawn records or versions",
    )?;
    ensure(
        matches!(
            review
                .detail(
                    &withdrawn_case.case_id,
                    &registry,
                    &org,
                    &all_records_access()
                )
                .await,
            Err(litecr_application::review::ReviewStoreError::NotFound)
        ),
        "review detail hides a case linked to withdrawn evidence",
    )?;
    ensure(
        review
            .assign(
                &reviewer,
                &withdrawn_case.case_id,
                &registry,
                &org,
                withdrawn_case.case_version,
                &reviewer.id,
            )
            .await
            .is_err(),
        "review assignment fails closed after source withdrawal",
    )?;
    ensure(
        review
            .open_case(&reviewer, &open(&source_ids[3], &source_ids[1]))
            .await
            .is_err(),
        "review creation fails closed on withdrawn sources",
    )?;
    let assigned = review
        .assign(
            &reviewer,
            &case.case_id,
            &registry,
            &org,
            case.case_version,
            &reviewer.id,
        )
        .await
        .map_err(|_| "review assignment failed")?;
    let nonmatch = ReviewDecisionCommand {
        case_id: case.case_id.clone(),
        expected_case_version: assigned.case_version,
        expected_query_receive_version: 1,
        expected_candidate_receive_version: 1,
        expected_query_person_version: 1,
        expected_candidate_person_version: 1,
        kind: ReviewDecisionKind::ConfirmedNonMatch,
        reason_code: "synthetic_nonmatch".into(),
        reason: "Synthetic acceptance test only".into(),
        idempotency_key: format!("nonmatch-idem-{key}"),
        merge_id: None,
    };
    let result = review
        .decide(&reviewer, &registry, &org, &nonmatch)
        .await
        .map_err(|_| "confirmed non-match decision failed")?;
    ensure(
        result.outcome == "accepted",
        "non-match decision should be committed",
    )?;
    let replay = review
        .decide(&reviewer, &registry, &org, &nonmatch)
        .await
        .map_err(|_| "same decision replay failed")?;
    ensure(replay.replayed, "exact decision replay must be idempotent")?;
    let forbidden = review
        .open_case(&reviewer, &open(&source_ids[1], &source_ids[0]))
        .await
        .map_err(|_| "opposite review case could not be opened")?;
    let forbidden_merge = ReviewDecisionCommand {
        case_id: forbidden.case_id.clone(),
        expected_case_version: forbidden.case_version,
        expected_query_receive_version: 1,
        expected_candidate_receive_version: 1,
        expected_query_person_version: 1,
        expected_candidate_person_version: 1,
        kind: ReviewDecisionKind::Merge,
        reason_code: "synthetic_forbidden".into(),
        reason: "must remain blocked by confirmed non-match".into(),
        idempotency_key: format!("blocked-merge-{key}"),
        merge_id: None,
    };
    ensure(
        review
            .decide(&reviewer, &registry, &org, &forbidden_merge)
            .await
            .is_err(),
        "confirmed non-match must prevent a later merge",
    )?;
    let merge_case = review
        .open_case(&reviewer, &open(&source_ids[1], &source_ids[2]))
        .await
        .map_err(|_| "merge review case could not be opened")?;
    let merge = ReviewDecisionCommand {
        case_id: merge_case.case_id.clone(),
        expected_case_version: 1,
        expected_query_receive_version: 1,
        expected_candidate_receive_version: 1,
        expected_query_person_version: 1,
        expected_candidate_person_version: 1,
        kind: ReviewDecisionKind::Merge,
        reason_code: "synthetic_merge".into(),
        reason: "reviewed synthetic pair".into(),
        idempotency_key: format!("merge-idem-{key}"),
        merge_id: None,
    };
    let merged = review
        .decide(&reviewer, &registry, &org, &merge)
        .await
        .map_err(|_| "explicit merge failed")?;
    let merge_id = merged.merge_id.clone().ok_or("merge event id missing")?;
    let redirected = records
        .person(&registry, &org, &public_ids[1])
        .await
        .map_err(|_| "registry alias lookup failed")?
        .ok_or("merged alias was not resolvable")?;
    ensure(
        redirected.public_id == public_ids[2],
        "former public ID must redirect to survivor",
    )?;
    // Reversal is a second, versioned decision on the original merge case.
    let reverse_case = review
        .open_case(&reviewer, &open(&source_ids[1], &source_ids[2]))
        .await
        .map_err(|_| "reversal case could not be opened")?;
    let current_version = reverse_case.query_person_version;
    let reversal = ReviewDecisionCommand {
        case_id: reverse_case.case_id.clone(),
        expected_case_version: reverse_case.case_version,
        expected_query_receive_version: 1,
        expected_candidate_receive_version: 1,
        expected_query_person_version: current_version,
        expected_candidate_person_version: current_version,
        kind: ReviewDecisionKind::ReverseMerge,
        reason_code: "synthetic_reverse".into(),
        reason: "compensate merge in isolated test".into(),
        idempotency_key: format!("reverse-idem-{key}"),
        merge_id: Some(merge_id.clone()),
    };
    review
        .decide(&reviewer, &registry, &org, &reversal)
        .await
        .map_err(|error| format!("safe compensating reversal failed: {error:?}"))?;
    let history = review
        .detail(
            &reverse_case.case_id,
            &registry,
            &org,
            &all_records_access(),
        )
        .await
        .map_err(|error| format!("review history read failed: {error:?}"))?;
    ensure(
        history.history.len() == 2,
        "merge and reversal decisions should both remain in history",
    )?;
    ensure(
        history.history.iter().any(|item| {
            item.decision == "merge"
                && item.reason_code == "synthetic_merge"
                && item.merge_id.as_deref() == Some(merge_id.as_str())
        }),
        "history must retain merge reason and merge reference",
    )?;
    ensure(
        history.history.iter().any(|item| {
            item.decision == "reverse_merge"
                && item.reason_code == "synthetic_reverse"
                && item.actor_id == reviewer.id
        }),
        "history must expose reversal actor and reason code",
    )?;
    let restored = records
        .person(&registry, &org, &public_ids[1])
        .await
        .map_err(|_| "restored alias lookup failed")?
        .ok_or("restored original person missing")?;
    ensure(
        restored.public_id == public_ids[1],
        "reversal must restore original membership with source public ID",
    )?;

    // A source edit after a later merge changes both the captured membership
    // receive version and person resource version. Reversal must then require
    // an explicit review instead of restoring a stale membership snapshot.
    let remerge_case = review
        .open_case(&reviewer, &open(&source_ids[0], &source_ids[2]))
        .await
        .map_err(|_| "post-reversal synthetic remerge case could not be opened")?;
    let remerge = ReviewDecisionCommand {
        case_id: remerge_case.case_id.clone(),
        expected_case_version: remerge_case.case_version,
        expected_query_receive_version: remerge_case.query_receive_version,
        expected_candidate_receive_version: remerge_case.candidate_receive_version,
        expected_query_person_version: remerge_case.query_person_version,
        expected_candidate_person_version: remerge_case.candidate_person_version,
        kind: ReviewDecisionKind::Merge,
        reason_code: "synthetic_remerge_for_edit_guard".into(),
        reason: "Synthetic setup for changed-source reversal guard.".into(),
        idempotency_key: format!("remerge-idem-{key}"),
        merge_id: None,
    };
    let remerged = review
        .decide(&reviewer, &registry, &org, &remerge)
        .await
        .map_err(|error| format!("synthetic remerge failed: {error:?}"))?;
    let remerge_id = remerged
        .merge_id
        .ok_or("synthetic remerge omitted merge ID")?;

    let edited_source = SourceRecordCommand {
        registry_id: registry.clone(),
        organization_id: org.clone(),
        source_system_id: format!("src-{key}-a"),
        source_record_key: format!("key-{key}-a"),
        source_version: Some("v2-after-merge".into()),
        assertions: vec![SourceAssertion {
            field_path: "name.original".into(),
            original_value: format!("Synthetic edited b {key} only"),
            language_tag: Some("en".into()),
            script: Some("Latn".into()),
        }],
        identifiers: vec![],
    };
    let edited = records
        .ingest(
            &registrar,
            &format!("ingest-edit-after-merge-{key}"),
            &edited_source,
        )
        .await
        .map_err(|error| format!("synthetic source edit after merge failed: {error:?}"))?;
    ensure(
        edited.source_receive_version == 2,
        "source edit after merge must create a new accepted receive version",
    )?;
    let changed_case = review
        .open_case(
            &reviewer,
            &OpenReviewCase {
                registry_id: registry.clone(),
                organization_id: org.clone(),
                query_source_record_id: source_ids[0].clone(),
                candidate_source_record_id: source_ids[2].clone(),
                evidence_run_id: None,
                expected_query_receive_version: 2,
                expected_candidate_receive_version: 1,
            },
        )
        .await
        .map_err(|_| "post-edit synthetic reversal case could not be opened")?;
    let changed_reversal = ReviewDecisionCommand {
        case_id: changed_case.case_id.clone(),
        expected_case_version: changed_case.case_version,
        expected_query_receive_version: changed_case.query_receive_version,
        expected_candidate_receive_version: changed_case.candidate_receive_version,
        expected_query_person_version: changed_case.query_person_version,
        expected_candidate_person_version: changed_case.candidate_person_version,
        kind: ReviewDecisionKind::ReverseMerge,
        reason_code: "synthetic_reverse_after_source_edit".into(),
        reason: "Changed source requires a human-reviewed compensation.".into(),
        idempotency_key: format!("reverse-after-edit-{key}"),
        merge_id: Some(remerge_id),
    };
    ensure(
        matches!(
            review
                .decide(&reviewer, &registry, &org, &changed_reversal)
                .await,
            Err(litecr_application::review::ReviewStoreError::RequiresReview)
        ),
        "source edit after merge must refuse automatic reversal and require review",
    )?;
    Ok(())
}

fn ensure(ok: bool, msg: &str) -> Result<(), String> {
    if ok { Ok(()) } else { Err(msg.into()) }
}

fn decision_label(kind: &ReviewDecisionKind) -> Result<&'static str, String> {
    match kind {
        ReviewDecisionKind::Merge => Ok("merge"),
        ReviewDecisionKind::Link => Ok("link"),
        ReviewDecisionKind::ConfirmedNonMatch => Ok("confirmed_non_match"),
        _ => Err("unsupported synthetic competing-decision kind".into()),
    }
}

async fn cleanup(url: &str, key: &str) {
    let pool = connect(url, 2)
        .await
        .expect("synthetic test DB remains available");
    let registry = format!("review-reg-{key}");
    let org = format!("review-org-{key}");
    let mut tx = pool.begin().await.expect("cleanup begins");
    sqlx::query(
        "DELETE FROM litecr.consumer_receipt r USING litecr.outbox_event o \
         WHERE r.event_id=o.event_id AND o.registry_id=$1 AND o.organization_id=$2 \
           AND o.actor_id IN ($3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14)",
    )
    .bind(&registry)
    .bind(&org)
    .bind(format!("synthetic-reg-{key}"))
    .bind(format!("synthetic-reviewer-{key}"))
    .bind(format!("synthetic-race-reg-{key}"))
    .bind(format!("synthetic-race-opener-{key}"))
    .bind(format!("synthetic-race-merge-{key}"))
    .bind(format!("synthetic-race-link-{key}"))
    .bind(format!("synthetic-race-nonmatch-{key}"))
    .bind(format!("synthetic-page-registrar-{key}"))
    .bind(format!("synthetic-page-reviewer-{key}"))
    .bind(format!("synthetic-review-detail-registrar-{key}"))
    .bind(format!("synthetic-review-assigned-{key}"))
    .bind(format!("synthetic-review-assigner-{key}"))
    .execute(&mut *tx)
    .await
    .expect("delete only exact synthetic actors' worker receipts");
    sqlx::query(
        "DELETE FROM litecr.consumer_job j USING litecr.outbox_event o \
         WHERE j.event_id=o.event_id AND o.registry_id=$1 AND o.organization_id=$2 \
           AND o.actor_id IN ($3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14)",
    )
    .bind(&registry)
    .bind(&org)
    .bind(format!("synthetic-reg-{key}"))
    .bind(format!("synthetic-reviewer-{key}"))
    .bind(format!("synthetic-race-reg-{key}"))
    .bind(format!("synthetic-race-opener-{key}"))
    .bind(format!("synthetic-race-merge-{key}"))
    .bind(format!("synthetic-race-link-{key}"))
    .bind(format!("synthetic-race-nonmatch-{key}"))
    .bind(format!("synthetic-page-registrar-{key}"))
    .bind(format!("synthetic-page-reviewer-{key}"))
    .bind(format!("synthetic-review-detail-registrar-{key}"))
    .bind(format!("synthetic-review-assigned-{key}"))
    .bind(format!("synthetic-review-assigner-{key}"))
    .execute(&mut *tx)
    .await
    .expect("delete only exact synthetic actors' worker jobs");
    sqlx::query("DELETE FROM litecr.outbox_event WHERE registry_id=$1 AND organization_id=$2")
        .bind(&registry)
        .bind(&org)
        .execute(&mut *tx)
        .await
        .expect("delete only exact synthetic outbox");
    sqlx::query("DELETE FROM litecr.mutation_audit WHERE registry_id=$1 AND organization_id=$2")
        .bind(&registry)
        .bind(&org)
        .execute(&mut *tx)
        .await
        .expect("delete only exact synthetic audit");
    sqlx::query("DELETE FROM litecr.identity_merge_member WHERE merge_id IN (SELECT merge_id FROM litecr.identity_merge WHERE registry_id=$1 AND organization_id=$2)").bind(&registry).bind(&org).execute(&mut *tx).await.expect("delete synthetic merge membership snapshots");
    sqlx::query("DELETE FROM litecr.person_redirect WHERE registry_id=$1 AND organization_id=$2")
        .bind(&registry)
        .bind(&org)
        .execute(&mut *tx)
        .await
        .expect("delete synthetic redirects");
    sqlx::query("DELETE FROM litecr.identity_merge WHERE registry_id=$1 AND organization_id=$2")
        .bind(&registry)
        .bind(&org)
        .execute(&mut *tx)
        .await
        .expect("delete synthetic merges");
    sqlx::query("DELETE FROM litecr.identity_link WHERE registry_id=$1 AND organization_id=$2")
        .bind(&registry)
        .bind(&org)
        .execute(&mut *tx)
        .await
        .expect("delete synthetic links");
    sqlx::query(
        "DELETE FROM litecr.confirmed_non_match WHERE registry_id=$1 AND organization_id=$2",
    )
    .bind(&registry)
    .bind(&org)
    .execute(&mut *tx)
    .await
    .expect("delete synthetic non-matches");
    sqlx::query("DELETE FROM litecr.review_decision WHERE case_id IN (SELECT case_id FROM litecr.review_case WHERE registry_id=$1 AND organization_id=$2)").bind(&registry).bind(&org).execute(&mut *tx).await.expect("delete synthetic decisions");
    sqlx::query("DELETE FROM litecr.review_case WHERE registry_id=$1 AND organization_id=$2")
        .bind(&registry)
        .bind(&org)
        .execute(&mut *tx)
        .await
        .expect("delete synthetic cases");
    sqlx::query("DELETE FROM litecr.registry_idempotency WHERE registry_id=$1")
        .bind(&registry)
        .execute(&mut *tx)
        .await
        .expect("delete synthetic idempotency records");
    sqlx::query("DELETE FROM litecr.person_identifier WHERE registry_id=$1 AND organization_id=$2")
        .bind(&registry)
        .bind(&org)
        .execute(&mut *tx)
        .await
        .expect("delete synthetic identifiers");
    sqlx::query("DELETE FROM litecr.source_identifier_assertion WHERE registry_id=$1 AND organization_id=$2").bind(&registry).bind(&org).execute(&mut *tx).await.expect("delete synthetic identifier assertions");
    sqlx::query("DELETE FROM matching.normalized_assertion WHERE source_record_id IN (SELECT source_record_id FROM litecr.source_record WHERE registry_id=$1 AND organization_id=$2)").bind(&registry).bind(&org).execute(&mut *tx).await.expect("delete synthetic normalized assertions");
    sqlx::query(
        "DELETE FROM matching.normalized_identifier WHERE registry_id=$1 AND organization_id=$2",
    )
    .bind(&registry)
    .bind(&org)
    .execute(&mut *tx)
    .await
    .expect("delete synthetic normalized identifiers");
    sqlx::query("DELETE FROM matching.indexed_source_version WHERE source_record_id IN (SELECT source_record_id FROM litecr.source_record WHERE registry_id=$1 AND organization_id=$2)").bind(&registry).bind(&org).execute(&mut *tx).await.expect("delete synthetic matching index markers");
    sqlx::query("DELETE FROM litecr.source_assertion WHERE source_record_id IN (SELECT source_record_id FROM litecr.source_record WHERE registry_id=$1 AND organization_id=$2)").bind(&registry).bind(&org).execute(&mut *tx).await.expect("delete synthetic assertions");
    sqlx::query("DELETE FROM litecr.source_record_version WHERE source_record_id IN (SELECT source_record_id FROM litecr.source_record WHERE registry_id=$1 AND organization_id=$2)").bind(&registry).bind(&org).execute(&mut *tx).await.expect("delete synthetic source versions");
    sqlx::query("DELETE FROM litecr.source_record WHERE registry_id=$1 AND organization_id=$2")
        .bind(&registry)
        .bind(&org)
        .execute(&mut *tx)
        .await
        .expect("delete synthetic source records");
    sqlx::query("DELETE FROM litecr.person_alias WHERE registry_id=$1 AND organization_id=$2")
        .bind(&registry)
        .bind(&org)
        .execute(&mut *tx)
        .await
        .expect("delete synthetic aliases");
    sqlx::query("DELETE FROM litecr.person WHERE registry_id=$1 AND organization_id=$2")
        .bind(&registry)
        .bind(&org)
        .execute(&mut *tx)
        .await
        .expect("delete synthetic people");
    let review_detail_subjects = vec![
        format!("synthetic-review-assigned-{key}"),
        format!("synthetic-review-assigner-{key}"),
        format!("synthetic-review-one-record-{key}"),
        format!("synthetic-review-both-records-{key}"),
        format!("synthetic-review-inactive-{key}"),
        format!("synthetic-review-wrong-org-{key}"),
    ];
    sqlx::query(
        "DELETE FROM litecr.access_session WHERE actor_kind='human' AND issuer_url=$1 AND external_subject=ANY($2)",
    )
    .bind(SYNTHETIC_ISSUER)
    .bind(&review_detail_subjects)
    .execute(&mut *tx)
    .await
    .expect("delete only synthetic review sessions");
    sqlx::query(
        "DELETE FROM litecr.access_grant WHERE actor_kind='human' AND issuer_url=$1 AND external_subject=ANY($2)",
    )
    .bind(SYNTHETIC_ISSUER)
    .bind(&review_detail_subjects)
    .execute(&mut *tx)
    .await
    .expect("delete only synthetic review grants");
    sqlx::query(
        "DELETE FROM litecr.access_identity WHERE actor_kind='human' AND issuer_url=$1 AND external_subject=ANY($2)",
    )
    .bind(SYNTHETIC_ISSUER)
    .bind(&review_detail_subjects)
    .execute(&mut *tx)
    .await
    .expect("delete only synthetic review identities");
    sqlx::query(
        "DELETE FROM litecr.access_grant WHERE actor_kind='integration_client' AND issuer_url='' AND external_subject=$1",
    )
    .bind(format!("synthetic-review-integration-only-{key}"))
    .execute(&mut *tx)
    .await
    .expect("delete only synthetic integration-only target grant");
    sqlx::query(
        "DELETE FROM litecr.access_identity WHERE actor_kind='integration_client' AND issuer_url='' AND external_subject=$1",
    )
    .bind(format!("synthetic-review-integration-only-{key}"))
    .execute(&mut *tx)
    .await
    .expect("delete only synthetic integration-only target identity");
    tx.commit().await.expect("cleanup commits");
}
