use litecr_application::{
    access::ActorKind,
    registry::RegistryStore,
    review::{ReviewActor, ReviewDetailAccess, ReviewQueueFilter, ReviewStore, ReviewStoreError},
};
use litecr_domain::{
    registry::{SourceAssertion, SourceRecordCommand},
    review::OpenReviewCase,
};
use litecr_infrastructure::{
    connect, migrate, registry::PostgresRegistryStore, review::PostgresReviewStore,
};

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
async fn review_case_persists_and_exposes_only_exact_match_evidence_provenance() {
    let url = std::env::var("LITECR_TEST_DATABASE_URL")
        .expect("set LITECR_TEST_DATABASE_URL to an isolated synthetic database");
    let key = suffix();
    let task = tokio::spawn(scenario(url.clone(), key.clone())).await;
    cleanup(&url, &key).await;
    assert!(task.is_ok(), "provenance scenario task panicked: {task:?}");
    task.unwrap().expect("review evidence provenance scenario");
}

async fn scenario(url: String, key: String) -> Result<(), String> {
    let pool = connect(&url, 5)
        .await
        .map_err(|_| "synthetic provenance DB unavailable")?;
    migrate(&pool)
        .await
        .map_err(|_| "synthetic provenance migrations unavailable")?;
    let registry = format!("review-prov-reg-{key}");
    let org = format!("review-prov-org-{key}");
    let actor = ReviewActor {
        id: format!("review-prov-actor-{key}"),
        issuer_url: Some("https://review-idp.example.test".into()),
        request_id: format!("review-prov-request-{key}"),
        correlation_id: None,
    };
    let registrar = litecr_application::registry::RegistryActor {
        kind: ActorKind::Human,
        id: actor.id.clone(),
        issuer_url: actor.issuer_url.clone(),
        request_id: actor.request_id.clone(),
        correlation_id: None,
    };
    let records = PostgresRegistryStore::new(pool.clone());
    let mut ids = Vec::new();
    for (label, version) in [("query", "q1"), ("candidate", "c1"), ("other", "o1")] {
        let receipt = records
            .ingest(
                &registrar,
                &format!("prov-ingest-{key}-{label}-{version}"),
                &SourceRecordCommand {
                    registry_id: registry.clone(),
                    organization_id: org.clone(),
                    source_system_id: format!("prov-source-{key}-{label}"),
                    source_record_key: format!("prov-key-{key}-{label}"),
                    source_version: Some(version.into()),
                    assertions: vec![SourceAssertion {
                        field_path: "name.original".into(),
                        original_value: format!("Synthetic provenance {key} {label}"),
                        language_tag: Some("en".into()),
                        script: Some("Latn".into()),
                    }],
                    identifiers: vec![],
                },
            )
            .await
            .map_err(|_| "synthetic source record seed failed")?;
        ids.push(receipt.source_record_id);
    }
    records
        .ingest(
            &registrar,
            &format!("prov-ingest-{key}-candidate-c2"),
            &SourceRecordCommand {
                registry_id: registry.clone(),
                organization_id: org.clone(),
                source_system_id: format!("prov-source-{key}-candidate"),
                source_record_key: format!("prov-key-{key}-candidate"),
                source_version: Some("c2".into()),
                assertions: vec![SourceAssertion {
                    field_path: "name.original".into(),
                    original_value: format!("Synthetic provenance {key} candidate revised"),
                    language_tag: Some("en".into()),
                    script: Some("Latn".into()),
                }],
                identifiers: vec![],
            },
        )
        .await
        .map_err(|_| "synthetic candidate update failed")?;

    let valid_run = evidence_run(&pool, &registry, &org, &ids[0], 1, &key, "valid").await?;
    evidence_candidate(&pool, &valid_run, &ids[1], 2).await?;
    let unrelated_run = evidence_run(&pool, &registry, &org, &ids[2], 1, &key, "unrelated").await?;
    evidence_candidate(&pool, &unrelated_run, &ids[1], 2).await?;
    let stale_run = evidence_run(&pool, &registry, &org, &ids[0], 1, &key, "stale").await?;
    evidence_candidate(&pool, &stale_run, &ids[1], 1).await?;

    let review = PostgresReviewStore::new(pool.clone());
    let open = |run_id: &str| OpenReviewCase {
        registry_id: registry.clone(),
        organization_id: org.clone(),
        query_source_record_id: ids[0].clone(),
        candidate_source_record_id: ids[1].clone(),
        expected_query_receive_version: 1,
        expected_candidate_receive_version: 2,
        evidence_run_id: Some(run_id.to_owned()),
    };
    let valid = review
        .open_case(&actor, &open(&valid_run))
        .await
        .map_err(|_| "valid evidence-bound review case was rejected")?;
    if valid.evidence_run_id.as_deref() != Some(valid_run.as_str()) {
        return Err("review case did not return the persisted evidence run ID".into());
    }
    let detail = review
        .detail(&valid.case_id, &registry, &org, &all_records_access())
        .await
        .map_err(|_| "review case detail failed")?;
    if detail.case.evidence_run_id.as_deref() != Some(valid_run.as_str()) {
        return Err("review detail omitted evidence provenance".into());
    }
    let listed = review
        .list_cases(&registry, &org, ReviewQueueFilter::All, 10, None)
        .await
        .map_err(|_| "review case queue failed")?;
    if listed
        .cases
        .iter()
        .find(|case| case.case_id == valid.case_id)
        .and_then(|case| case.evidence_run_id.as_deref())
        != Some(valid_run.as_str())
    {
        return Err("review queue omitted evidence provenance".into());
    }

    for (label, run_id) in [("unrelated", unrelated_run), ("stale", stale_run)] {
        match review.open_case(&actor, &open(&run_id)).await {
            Err(ReviewStoreError::EvidenceMismatch) => {}
            _ => return Err(format!("{label} evidence was not rejected")),
        }
    }
    pool.close().await;
    Ok(())
}

async fn evidence_run(
    pool: &sqlx::PgPool,
    registry: &str,
    org: &str,
    query_id: &str,
    query_version: i64,
    key: &str,
    label: &str,
) -> Result<String, String> {
    sqlx::query_scalar("INSERT INTO matching.evidence_run(registry_id,organization_id,query_source_record_id,query_receive_version,engine_version,normalization_version,candidate_version,feature_version,policy_version,source_snapshot_digest,candidate_count,candidates_truncated,actor_id,request_id) VALUES($1,$2,$3::uuid,$4,'test-engine','test-normalizer','test-candidates','test-features','test-policy',$5,1,FALSE,$6,$7) RETURNING run_id::text")
        .bind(registry).bind(org).bind(query_id).bind(query_version).bind(vec![7_u8;32]).bind(format!("actor-{key}")).bind(format!("request-{key}-{label}"))
        .fetch_one(pool).await.map_err(|_| "synthetic evidence run insert failed".into())
}

async fn evidence_candidate(
    pool: &sqlx::PgPool,
    run_id: &str,
    candidate_id: &str,
    candidate_version: i64,
) -> Result<(), String> {
    sqlx::query("INSERT INTO matching.evidence_candidate(run_id,candidate_source_record_id,candidate_receive_version,score_points,score_type,advisory_outcome,evidence) VALUES($1::uuid,$2::uuid,$3,50,'raw_heuristic_points_not_probability','review','{}'::jsonb)")
        .bind(run_id).bind(candidate_id).bind(candidate_version).execute(pool).await.map_err(|_| String::from("synthetic evidence candidate insert failed"))?;
    Ok(())
}

async fn cleanup(url: &str, key: &str) {
    let Ok(pool) = connect(url, 2).await else {
        return;
    };
    let registry = format!("review-prov-reg-{key}");
    let org = format!("review-prov-org-{key}");
    let mut tx = pool
        .begin()
        .await
        .expect("provenance cleanup transaction begins");
    sqlx::query("DELETE FROM litecr.review_case WHERE registry_id=$1 AND organization_id=$2")
        .bind(&registry)
        .bind(&org)
        .execute(&mut *tx)
        .await
        .expect("delete synthetic cases");
    sqlx::query("DELETE FROM matching.evidence_candidate WHERE run_id IN (SELECT run_id FROM matching.evidence_run WHERE registry_id=$1 AND organization_id=$2)")
        .bind(&registry).bind(&org).execute(&mut *tx).await.expect("delete synthetic evidence candidates");
    sqlx::query("DELETE FROM matching.evidence_run WHERE registry_id=$1 AND organization_id=$2")
        .bind(&registry)
        .bind(&org)
        .execute(&mut *tx)
        .await
        .expect("delete synthetic evidence runs");
    sqlx::query("DELETE FROM litecr.consumer_receipt WHERE event_id IN (SELECT event_id FROM litecr.consumer_job WHERE registry_id=$1 AND organization_id=$2)")
        .bind(&registry).bind(&org).execute(&mut *tx).await.expect("delete synthetic worker receipts");
    sqlx::query("DELETE FROM litecr.consumer_job WHERE registry_id=$1 AND organization_id=$2")
        .bind(&registry)
        .bind(&org)
        .execute(&mut *tx)
        .await
        .expect("delete synthetic worker jobs");
    sqlx::query("DELETE FROM litecr.outbox_event WHERE registry_id=$1 AND organization_id=$2")
        .bind(&registry)
        .bind(&org)
        .execute(&mut *tx)
        .await
        .expect("delete synthetic outbox events");
    sqlx::query("DELETE FROM litecr.mutation_audit WHERE registry_id=$1 AND organization_id=$2")
        .bind(&registry)
        .bind(&org)
        .execute(&mut *tx)
        .await
        .expect("delete synthetic mutation audit");
    sqlx::query("DELETE FROM litecr.registry_idempotency WHERE registry_id=$1")
        .bind(&registry)
        .execute(&mut *tx)
        .await
        .expect("delete synthetic idempotency keys");
    sqlx::query("DELETE FROM matching.normalized_assertion WHERE source_record_id IN (SELECT source_record_id FROM litecr.source_record WHERE registry_id=$1 AND organization_id=$2)")
        .bind(&registry).bind(&org).execute(&mut *tx).await.expect("delete synthetic normalized assertions");
    sqlx::query(
        "DELETE FROM matching.normalized_identifier WHERE registry_id=$1 AND organization_id=$2",
    )
    .bind(&registry)
    .bind(&org)
    .execute(&mut *tx)
    .await
    .expect("delete synthetic normalized identifiers");
    sqlx::query("DELETE FROM matching.indexed_source_version WHERE source_record_id IN (SELECT source_record_id FROM litecr.source_record WHERE registry_id=$1 AND organization_id=$2)")
        .bind(&registry).bind(&org).execute(&mut *tx).await.expect("delete synthetic matching index markers");
    sqlx::query("DELETE FROM litecr.source_assertion WHERE source_record_id IN (SELECT source_record_id FROM litecr.source_record WHERE registry_id=$1 AND organization_id=$2)")
        .bind(&registry).bind(&org).execute(&mut *tx).await.expect("delete synthetic assertions");
    sqlx::query("DELETE FROM litecr.source_identifier_assertion WHERE registry_id=$1 AND organization_id=$2")
        .bind(&registry).bind(&org).execute(&mut *tx).await.expect("delete synthetic identifier assertions");
    sqlx::query("DELETE FROM litecr.source_record_version WHERE source_record_id IN (SELECT source_record_id FROM litecr.source_record WHERE registry_id=$1 AND organization_id=$2)")
        .bind(&registry).bind(&org).execute(&mut *tx).await.expect("delete synthetic source versions");
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
    sqlx::query("DELETE FROM litecr.person_identifier WHERE registry_id=$1 AND organization_id=$2")
        .bind(&registry)
        .bind(&org)
        .execute(&mut *tx)
        .await
        .expect("delete synthetic identifiers");
    sqlx::query("DELETE FROM litecr.person WHERE registry_id=$1 AND organization_id=$2")
        .bind(&registry)
        .bind(&org)
        .execute(&mut *tx)
        .await
        .expect("delete synthetic people");
    tx.commit().await.expect("provenance cleanup commits");
    pool.close().await;
}
