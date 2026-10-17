//! Explicitly invoked synthetic load probe. No population-accuracy or SLA claim.
use litecr_application::{
    access::ActorKind,
    matching::MatchingStore,
    registry::{RegistryActor, RegistryStore},
};
use litecr_domain::registry::{SourceAssertion, SourceRecordCommand};
use litecr_infrastructure::{
    connect, matching::PostgresMatchingStore, migrate, registry::PostgresRegistryStore,
};
use litecr_matching::{AdvisoryOutcome, Pass};
use std::time::Instant;

#[tokio::test]
#[ignore = "requires a dedicated disposable cluster and an explicit synthetic load run"]
async fn synthetic_tolerant_name_load_probe() {
    let url =
        std::env::var("LITECR_TEST_DATABASE_URL").expect("dedicated synthetic database required");
    let count: usize = std::env::var("LITECR_MATCH_LOAD_RECORDS")
        .unwrap_or_else(|_| "1000".into())
        .parse()
        .unwrap();
    assert!(
        (100..=9000).contains(&count),
        "probe must remain within the matcher input budget"
    );
    let pool = connect(&url, 4).await.unwrap();
    migrate(&pool).await.unwrap();
    let registry = format!("synthetic-load-{}-{count}", std::process::id());
    let actor = RegistryActor {
        kind: ActorKind::System,
        id: "synthetic-load-seeder".into(),
        issuer_url: None,
        request_id: registry.clone(),
        correlation_id: None,
    };
    let store = PostgresRegistryStore::new(pool.clone());
    let matcher = PostgresMatchingStore::new(pool.clone());
    let mut controls = Vec::new();
    let started = Instant::now();
    let samples = [
        "ບຸນມີ ແກ້ວມະນີ",
        "ວັນດີ ສີສຸວັນ",
        "Michael Brown",
        "สมชาย ใจดี",
        "रमेश शर्मा",
        "أحمد سالم",
    ];
    for i in 0..count + 4 {
        let name = match i {
            0 => "ສົມໃຈ ໄຊຍະວົງ".to_owned(),
            1 => "ສົມໃຈ ໄຊຍະວົງ".to_owned(),
            2 => "ສົມໄຈ ໄຊຍະວົງ".to_owned(),
            3 => "ໄຊຍະວົງ".to_owned(),
            _ if i % 10 == 0 => format!("ບຸນມີ{i:06} ໄຊຍະວົງ"),
            _ => format!("{} {i:06}", samples[i % samples.len()]),
        };
        let command = SourceRecordCommand {
            registry_id: registry.clone(),
            organization_id: "synthetic-load-org".into(),
            source_system_id: "synthetic-load-source".into(),
            source_record_key: format!("load-{i}"),
            source_version: Some("invented-v1".into()),
            assertions: vec![SourceAssertion {
                field_path: "name.original".into(),
                original_value: name,
                language_tag: None,
                script: None,
            }],
            identifiers: vec![],
        };
        let row = store
            .ingest(&actor, &format!("{registry}-{i}"), &command)
            .await
            .unwrap();
        if i < 4 {
            controls.push(row.source_record_id);
        }
    }
    let ingest_ms = started.elapsed().as_secs_f64() * 1000.0;
    let index_started = Instant::now();
    let mut indexed = 0;
    loop {
        let batch = matcher
            .reindex_next_batch(Some(&registry), Some("synthetic-load-org"), 1000)
            .await
            .unwrap();
        indexed += batch;
        if batch == 0 {
            break;
        }
    }
    let indexed_records: i64 = sqlx::query_scalar("SELECT count(*) FROM matching.indexed_source_version ix JOIN litecr.source_record sr ON sr.source_record_id=ix.source_record_id AND sr.current_receive_version=ix.receive_version WHERE sr.registry_id=$1 AND sr.organization_id='synthetic-load-org' AND ix.normalizer_version=$2")
        .bind(&registry).bind(litecr_matching::NORMALIZATION_VERSION).fetch_one(&pool).await.unwrap();
    assert_eq!(
        indexed_records,
        (count + 4) as i64,
        "probe must exercise indexed retrieval, not only legacy fallback"
    );
    let index_ms = index_started.elapsed().as_secs_f64() * 1000.0;
    // Refresh planner statistics; this is a small disposable load rehearsal.
    sqlx::query("ANALYZE").execute(&pool).await.unwrap();
    let mut measurements = Vec::new();
    for (kind, query_index) in [("full_name", 0), ("surname_only", 3)] {
        for repetition in 0..5 {
            let started = Instant::now();
            let result = matcher
                .evaluate(
                    &registry,
                    "synthetic-load-org",
                    &controls[query_index],
                    200,
                    "synthetic-load-reviewer",
                    &format!("probe-{kind}-{repetition}"),
                    None,
                )
                .await
                .unwrap();
            let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
            if query_index == 0 {
                let exact = result
                    .candidates
                    .iter()
                    .find(|c| c.candidate_record_id == controls[1])
                    .expect("exact candidate retained under load");
                assert!(exact.passes.contains(&Pass::ExactName));
                let typo = result
                    .candidates
                    .iter()
                    .find(|c| c.candidate_record_id == controls[2])
                    .expect("native typo retained under load");
                assert!(typo.passes.contains(&Pass::NameVariant));
                assert_eq!(typo.outcome, AdvisoryOutcome::InsufficientEvidence);
                let partial = result
                    .candidates
                    .iter()
                    .find(|c| c.candidate_record_id == controls[3])
                    .expect("incomplete surname candidate retained under load");
                assert!(partial.passes.contains(&Pass::IncompleteName));
            } else {
                let expected_union = 3 + (count + 3) / 10;
                assert_eq!(
                    result.retrieval.union_before_limit, expected_union,
                    "all constructed shared-surname candidates must be retrieved before the visible cap"
                );
                assert_eq!(result.candidates.len(), expected_union.min(200));
                assert!(result.candidates.iter().all(|c| c.score_points == 0
                    && c.outcome == AdvisoryOutcome::InsufficientEvidence
                    && c.passes.contains(&Pass::IncompleteName)));
            }
            assert_eq!(
                result.retrieval.truncated,
                result.retrieval.union_before_limit > 200
            );
            measurements.push(serde_json::json!({"query_kind":kind,"repetition":repetition,"elapsed_ms":elapsed_ms,"returned":result.candidates.len(),"union_before_limit":result.retrieval.union_before_limit,"truncated":result.retrieval.truncated}));
        }
    }
    let database_version: String = sqlx::query_scalar("SHOW server_version")
        .fetch_one(&pool)
        .await
        .unwrap();
    let collation: String =
        sqlx::query_scalar("SELECT datcollate FROM pg_database WHERE datname=current_database()")
            .fetch_one(&pool)
            .await
            .unwrap();
    let report = serde_json::json!({"synthetic_only":true,"schema_version":litecr_domain::SCHEMA_VERSION,"distractors":count,"population_records":count+4,"indexed_records":indexed_records,"reindexed_records":indexed,"database_version":database_version,"collation":collation,"ingest_elapsed_ms":ingest_ms,"index_elapsed_ms":index_ms,"measurements":measurements,"limits":"Single local same-host sequential probe, invented strings and labels, no HTTP/concurrency/production SLA or population accuracy claim."});
    let path =
        std::env::var("LITECR_MATCH_LOAD_REPORT").expect("explicit aggregate output path required");
    std::fs::write(path, serde_json::to_string_pretty(&report).unwrap() + "\n").unwrap();
    pool.close().await;
}
