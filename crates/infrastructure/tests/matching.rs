use litecr_application::registry::{RegistryActor, RegistryStore};
use litecr_application::{
    access::ActorKind,
    matching::{MatchIdempotency, MatchingStore, MatchingStoreError},
};
use litecr_domain::registry::{SourceAssertion, SourceRecordCommand};
use litecr_infrastructure::{
    connect, matching::PostgresMatchingStore, migrate, registry::PostgresRegistryStore,
};
use sqlx::{PgPool, Postgres, Row, Transaction};
use std::time::Duration;
use tokio::time::{Instant, sleep, timeout};

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

#[tokio::test]
async fn postgres_name_retrieval_guards_preserve_helper_positives() {
    let url = std::env::var("LITECR_TEST_DATABASE_URL").expect("synthetic-only test DB required");
    let pool = connect(&url, 2)
        .await
        .expect("synthetic guard test DB reachable");
    migrate(&pool).await.expect("guard test schema migrated");
    let controls: serde_json::Value = serde_json::from_str(include_str!(
        "../../../fixtures/matching/name-variant-controls-v1.json"
    ))
    .expect("checked-in name controls decode");
    let mut names: Vec<String> = controls["cases"]
        .as_array()
        .expect("name cases present")
        .iter()
        .flat_map(|case| {
            ["left", "right"].map(|side| {
                litecr_matching::normalized_assertion_key(
                    case[side].as_str().expect("name present"),
                )
            })
        })
        .collect();
    names.extend(
        [
            "",
            "ab",
            "abc",
            "abcd",
            "abdc",
            "abxcd",
            "acd",
            "abce",
            "abc abc",
            "abc abcd",
            "abcd abc",
            "abc xyz",
            "xyz abc",
            "abc abc xyz",
            "aaaa",
            "aaab",
            "bbbb",
            "anna ann",
            "annette anna",
            "anna anna",
            "ສົມໃຈ ສົມໃຈ",
            "ສົມໃຈ ສົມໄຈ",
            "ສົມໄຈ ສົມໃຈ",
            "éabc",
            "e\u{301}abc",
            "😀abc",
            "😀acb",
            "abc😀",
            "ab😀c",
        ]
        .into_iter()
        .map(str::to_owned),
    );
    names.push("a".repeat(128));
    names.push(format!("{}b", "a".repeat(127)));
    names.sort();
    names.dedup();
    assert!(names.len() <= 96, "keep Cartesian guard regression bounded");
    let parts_sql = format!(
        "WITH pairs AS MATERIALIZED (SELECT a,b FROM unnest($1::text[]) qa(a) CROSS JOIN unnest($1::text[]) cb(b)), \
         checked AS MATERIALIZED (SELECT matching.name_parts_within_v1(a,b) AS accepted, ({}) AS guarded FROM pairs) \
         SELECT count(*) FILTER (WHERE accepted AND NOT guarded), \
                count(*) FILTER (WHERE accepted), count(*) FILTER (WHERE NOT guarded), \
                count(*) FILTER (WHERE NOT accepted AND guarded) FROM checked",
        include_str!("../../../fixtures/matching/name-parts-guard-v1.sql").trim(),
    );
    let (lost, positives, pruned, conservative): (i64, i64, i64, i64) = sqlx::query_as(&parts_sql)
        .bind(&names)
        .fetch_one(&pool)
        .await
        .expect("Cartesian parts guard comparison executes");
    assert_eq!(
        lost, 0,
        "necessary prefix guard must retain every helper-positive pair"
    );
    assert!(
        positives > 0 && pruned > 0,
        "cover accepted and pruned pairs"
    );
    assert!(
        conservative > 0,
        "guard must still defer injective assignment to exact helper"
    );
    let edit_sql = "WITH checked AS MATERIALIZED ( \
        SELECT matching.name_edit_within_v1(a,b,k) AS accepted, \
               char_length(translate(a,b,''))<=k AND char_length(translate(b,a,''))<=k AS guarded \
        FROM unnest($1::text[]) qa(a) CROSS JOIN unnest($1::text[]) cb(b) CROSS JOIN generate_series(1,2) budget(k)) \
        SELECT count(*) FILTER (WHERE accepted AND NOT guarded), \
               count(*) FILTER (WHERE accepted), count(*) FILTER (WHERE NOT guarded) FROM checked";
    let (lost, positives, pruned): (i64, i64, i64) = sqlx::query_as(edit_sql)
        .bind(&names)
        .fetch_one(&pool)
        .await
        .expect("Cartesian one/two-edit guard comparison executes");
    assert_eq!(
        lost, 0,
        "absent-character guard must retain every helper-positive pair"
    );
    assert!(
        positives > 0 && pruned > 0,
        "cover accepted and pruned edit pairs"
    );
    pool.close().await;
}

#[tokio::test]
async fn postgres_tolerant_name_dedup_preserves_distinct_current_scoped_sources() {
    let url = std::env::var("LITECR_TEST_DATABASE_URL").expect("synthetic-only test DB required");
    let key = suffix();
    let run = tokio::spawn(tolerant_name_dedup_scenario(url.clone(), key.clone())).await;
    cleanup(&url, &key).await;
    run.expect("duplicate-name scenario joined")
        .expect("duplicate-name retrieval preserves eligible source identities");
}

async fn tolerant_name_dedup_scenario(url: String, key: String) -> Result<(), String> {
    let pool = connect(&url, 8).await.map_err(debug)?;
    migrate(&pool).await.map_err(debug)?;
    let registry = format!("match-reg-{key}");
    let org = format!("match-org-{key}");
    let other_org = format!("match-other-{key}");
    let actor = RegistryActor {
        kind: ActorKind::System,
        id: format!("synthetic-match-seeder-{key}"),
        issuer_url: None,
        request_id: format!("dedup-seed-{key}"),
        correlation_id: None,
    };
    let ingest = PostgresRegistryStore::new(pool.clone());
    let matcher = PostgresMatchingStore::new(pool.clone());
    let name_only = |scope: &str, record: &str, name: &str| {
        let mut cmd = command(&registry, scope, &key, record, name, "");
        cmd.assertions.truncate(1);
        cmd.assertions[0].language_tag = None;
        cmd.assertions[0].script = None;
        cmd
    };
    let mut query_cmd = name_only(&org, "query", "Somchai Phommasane");
    let mut duplicate_name = query_cmd.assertions[0].clone();
    duplicate_name.field_path = "name".into();
    query_cmd.assertions.push(duplicate_name);
    let query = ingest
        .ingest(&actor, &format!("dedup-query-{key}"), &query_cmd)
        .await
        .map_err(debug)?;
    let mut expected_ids = Vec::new();
    // Distinct people sharing a typo must remain distinct candidates. With no
    // DOB or identifiers, only tolerant-name retrieval can supply these rows.
    for i in 0..3 {
        let row = ingest
            .ingest(
                &actor,
                &format!("dedup-eligible-{i}-{key}"),
                &name_only(&org, &format!("eligible-{i}"), "Somchai Phommasnae"),
            )
            .await
            .map_err(debug)?;
        expected_ids.push(row.source_record_id);
    }
    expected_ids.sort();
    ingest
        .ingest(
            &actor,
            &format!("dedup-foreign-{key}"),
            &name_only(&other_org, "foreign", "Somchai Phommasnae"),
        )
        .await
        .map_err(debug)?;
    let withdrawn = ingest
        .ingest(
            &actor,
            &format!("dedup-withdrawn-{key}"),
            &name_only(&org, "withdrawn", "Somchai Phommasnae"),
        )
        .await
        .map_err(debug)?;
    sqlx::query(
        "UPDATE litecr.source_record SET withdrawn_at=now(),withdrawn_by_manifest=decode(repeat('00',32),'hex') WHERE source_record_id=$1::uuid",
    )
    .bind(&withdrawn.source_record_id)
    .execute(&pool)
    .await
    .map_err(debug)?;
    let superseded = ingest
        .ingest(
            &actor,
            &format!("dedup-old-{key}"),
            &name_only(&org, "superseded", "Somchai Phommasnae"),
        )
        .await
        .map_err(debug)?;
    let mut replacement = name_only(&org, "superseded", "Zzzzzzz Qqqqqqqq");
    replacement.source_version = Some("opaque-v2".into());
    let replacement = ingest
        .ingest(&actor, &format!("dedup-new-{key}"), &replacement)
        .await
        .map_err(debug)?;
    ensure(
        replacement.source_record_id == superseded.source_record_id
            && replacement.source_receive_version == 2,
        "superseded fixture must update the same source to receive version two",
    )?;
    let historical_rows: i64 = sqlx::query_scalar(
        "SELECT count(DISTINCT source_record_id) FROM matching.normalized_assertion WHERE source_record_id IN ($1::uuid,$2::uuid) AND receive_version=1 AND normalized_key='somchai phommasnae'",
    )
    .bind(&withdrawn.source_record_id)
    .bind(&superseded.source_record_id)
    .fetch_one(&pool)
    .await
    .map_err(debug)?;
    ensure(
        historical_rows == 2,
        "withdrawn and superseded fixtures must retain their old normalized keys",
    )?;
    for stage in ["indexed", "legacy", "reindexed"] {
        if stage == "legacy" {
            sqlx::query("DELETE FROM matching.indexed_source_version WHERE source_record_id IN (SELECT source_record_id FROM litecr.source_record WHERE registry_id=$1 AND organization_id=$2)")
                .bind(&registry).bind(&org).execute(&pool).await.map_err(debug)?;
        }
        if stage == "reindexed" {
            ensure(
                matcher
                    .reindex_next_batch(Some(&registry), Some(&org), 100)
                    .await
                    .map_err(debug)?
                    == 5,
                "reindex must cover query, three eligible sources and current replacement only",
            )?;
        }
        let actual = matcher
            .evaluate(
                &registry,
                &org,
                &query.source_record_id,
                50,
                "synthetic-matcher",
                &format!("dedup-{stage}-{key}"),
                None,
            )
            .await
            .map_err(debug)?;
        let mut actual_ids: Vec<_> = actual
            .candidates
            .iter()
            .map(|candidate| candidate.candidate_record_id.clone())
            .collect();
        actual_ids.sort();
        ensure(
            actual_ids == expected_ids,
            &format!("{stage} must expand the matching name to all and only eligible source IDs"),
        )?;
        ensure(
            actual.query_receive_version == Some(1)
                && actual.candidates.iter().all(|candidate| {
                    candidate.candidate_receive_version == Some(1)
                        && candidate.candidate_source_version.as_deref() == Some("opaque-v1")
                }),
            "deduplicating names must preserve each source's evidence version",
        )?;
    }
    pool.close().await;
    Ok(())
}

#[tokio::test]
async fn postgres_tolerant_names_match_pure_retrieval_before_and_after_reindex() {
    let url = std::env::var("LITECR_TEST_DATABASE_URL").expect("synthetic-only test DB required");
    let key = suffix();
    let run = tokio::spawn(tolerant_name_scenario(url.clone(), key.clone())).await;
    cleanup(&url, &key).await;
    run.expect("tolerant-name scenario joined")
        .expect("tolerant-name PostgreSQL parity");
}

async fn tolerant_name_scenario(url: String, key: String) -> Result<(), String> {
    let pool = connect(&url, 8).await.map_err(debug)?;
    migrate(&pool).await.map_err(debug)?;
    let (common_given, duplicate_part, distinct_assignment, over_limit): (bool,bool,bool,bool) = sqlx::query_as(
        "SELECT matching.name_parts_within_v1('Somchai UniqueSurname','Somchai DifferentSurname'), \
          matching.name_parts_within_v1('Somchai Somchai','Somchai Phommasane'), \
          matching.name_parts_within_v1('anna ann','annette anna'), \
          matching.name_edit_within_v1(repeat('a',129),repeat('a',129),2)"
    ).fetch_one(&pool).await.map_err(debug)?;
    ensure(
        !common_given && !duplicate_part && distinct_assignment && !over_limit,
        "SQL name helpers must reject one-token flooding/duplicate assignment/oversize and preserve valid distinct assignment",
    )?;
    let registry = format!("match-reg-{key}");
    let org = format!("match-org-{key}");
    let actor = RegistryActor {
        kind: ActorKind::System,
        id: format!("synthetic-match-seeder-{key}"),
        issuer_url: None,
        request_id: format!("variant-seed-{key}"),
        correlation_id: None,
    };
    let ingest = PostgresRegistryStore::new(pool.clone());
    let matcher = PostgresMatchingStore::new(pool.clone());
    let controls: serde_json::Value = serde_json::from_str(include_str!(
        "../../../fixtures/matching/name-variant-controls-v1.json"
    ))
    .map_err(debug)?;
    let cases = controls["cases"].as_array().ok_or("cases absent")?;
    let mut pure = Vec::new();
    let mut queries = Vec::new();
    for (i, case) in cases.iter().enumerate() {
        let mut ids = Vec::new();
        for side in ["left", "right"] {
            let name = case[side].as_str().ok_or("name absent")?;
            let label = format!("variant-{i}-{side}");
            let mut cmd = command(&registry, &org, &key, &label, name, "");
            cmd.assertions.truncate(1);
            cmd.assertions[0].script = None;
            cmd.assertions[0].language_tag = None;
            let row = ingest
                .ingest(&actor, &format!("{label}-{key}"), &cmd)
                .await
                .map_err(debug)?;
            pure.push(match_record(
                &row.source_record_id,
                &registry,
                &org,
                name,
                "",
                vec![],
            ));
            ids.push(row.source_record_id);
        }
        queries.push((
            ids[0].clone(),
            ids[1].clone(),
            case["retrieve"].as_bool().ok_or("label absent")?,
        ));
    }
    matcher
        .reindex_next_batch(Some(&registry), Some(&org), 200)
        .await
        .map_err(debug)?;
    for stage in ["indexed", "legacy", "reindexed"] {
        if stage == "legacy" {
            sqlx::query("DELETE FROM matching.indexed_source_version WHERE source_record_id IN (SELECT source_record_id FROM litecr.source_record WHERE registry_id=$1 AND organization_id=$2)").bind(&registry).bind(&org).execute(&pool).await.map_err(debug)?;
        }
        if stage == "reindexed" {
            ensure(
                matcher
                    .reindex_next_batch(Some(&registry), Some(&org), 200)
                    .await
                    .map_err(debug)?
                    == pure.len(),
                "all variant records must be reindexed",
            )?;
        }
        let indexed_count: i64 = sqlx::query_scalar("SELECT count(*) FROM matching.indexed_source_version ix JOIN litecr.source_record sr ON sr.source_record_id=ix.source_record_id AND sr.current_receive_version=ix.receive_version WHERE sr.registry_id=$1 AND sr.organization_id=$2 AND ix.normalizer_version=$3")
            .bind(&registry).bind(&org).bind(litecr_matching::NORMALIZATION_VERSION).fetch_one(&pool).await.map_err(debug)?;
        ensure(
            indexed_count
                == if stage == "legacy" {
                    0
                } else {
                    pure.len() as i64
                },
            "parity stage must prove its actual index coverage",
        )?;
        for (query, expected_id, retrieve) in &queries {
            let expected = litecr_matching::match_records(query, &pure, 200).map_err(debug)?;
            let actual = matcher
                .evaluate(
                    &registry,
                    &org,
                    query,
                    200,
                    "synthetic-matcher",
                    &format!("variant-{stage}-{query}"),
                    None,
                )
                .await
                .map_err(debug)?;
            ensure(
                actual.candidates == expected.candidates && actual.retrieval == expected.retrieval,
                &format!("{stage} tolerant-name retrieval/evidence differ from exhaustive Rust"),
            )?;
            ensure(
                actual
                    .candidates
                    .iter()
                    .any(|c| &c.candidate_record_id == expected_id)
                    == *retrieve,
                "designated spelling/partial pair retrieval differs from label",
            )?;
            let saved = matcher
                .read_evidence(
                    actual.evidence_run_id.as_deref().ok_or("run ID absent")?,
                    &registry,
                    &org,
                )
                .await
                .map_err(debug)?;
            ensure(
                saved == actual,
                "tolerant evidence must round-trip with versioned passes",
            )?;
        }
    }
    pool.close().await;
    Ok(())
}

#[tokio::test]
async fn postgres_matching_is_scoped_versioned_and_persists_advisory_lineage() {
    let url = std::env::var("LITECR_TEST_DATABASE_URL")
        .expect("set LITECR_TEST_DATABASE_URL to the synthetic-only test DB");
    let key = suffix();
    let run = tokio::spawn(scenario(url.clone(), key.clone())).await;
    cleanup(&url, &key).await;
    run.expect("matching scenario joined")
        .expect("matching database scenario");
}

#[tokio::test]
async fn postgres_matching_rejects_changed_query_or_candidate_before_evidence_insert() {
    let url = std::env::var("LITECR_TEST_DATABASE_URL")
        .expect("set LITECR_TEST_DATABASE_URL to the synthetic-only test DB");
    let key = suffix();
    let run = tokio::spawn(stale_snapshot_scenario(url.clone(), key.clone())).await;
    cleanup(&url, &key).await;
    run.expect("stale matching scenario joined")
        .expect("stale matching database scenario");
}

#[tokio::test]
async fn postgres_matching_idempotency_replays_conflicts_and_isolates_scope() {
    let url = std::env::var("LITECR_TEST_DATABASE_URL")
        .expect("set LITECR_TEST_DATABASE_URL to the synthetic-only test DB");
    let key = suffix();
    let run = tokio::spawn(idempotency_scenario(url.clone(), key.clone())).await;
    cleanup(&url, &key).await;
    run.expect("idempotency scenario joined")
        .expect("matching idempotency database scenario");
}

async fn idempotency_scenario(url: String, key: String) -> Result<(), String> {
    let pool = connect(&url, 8).await.map_err(debug)?;
    migrate(&pool).await.map_err(debug)?;
    let registry = format!("match-reg-{key}");
    let org = format!("match-org-{key}");
    let other_org = format!("match-other-{key}");
    let actor = RegistryActor {
        kind: ActorKind::System,
        id: format!("synthetic-match-seeder-{key}"),
        issuer_url: None,
        request_id: format!("match-idempotency-seed-{key}"),
        correlation_id: None,
    };
    let ingest = PostgresRegistryStore::new(pool.clone());
    let query = ingest
        .ingest(
            &actor,
            &format!("seed-idempotency-query-{key}"),
            &command(&registry, &org, &key, "query", "ສົມໃຈ", "1986-04"),
        )
        .await
        .map_err(debug)?;
    ingest
        .ingest(
            &actor,
            &format!("seed-idempotency-candidate-{key}"),
            &command(&registry, &org, &key, "candidate", "ສົມໃຈ", "1986-04-19"),
        )
        .await
        .map_err(debug)?;
    let other_query = ingest
        .ingest(
            &actor,
            &format!("seed-idempotency-other-org-{key}"),
            &command(
                &registry,
                &other_org,
                &key,
                "other-query",
                "ສົມໃຈ",
                "1986-04",
            ),
        )
        .await
        .map_err(debug)?;

    let matcher = PostgresMatchingStore::new(pool.clone());
    let idempotency = MatchIdempotency {
        actor_digest: [0x11; 32],
        key_digest: [0x22; 32],
        request_digest: [0x33; 32],
    };
    let (left, right) = tokio::join!(
        matcher.evaluate(
            &registry,
            &org,
            &query.source_record_id,
            20,
            "synthetic-matcher",
            "request-idempotency-left",
            Some(idempotency),
        ),
        matcher.evaluate(
            &registry,
            &org,
            &query.source_record_id,
            20,
            "synthetic-matcher",
            "request-idempotency-right",
            Some(idempotency),
        )
    );
    let left = left.map_err(debug)?;
    let right = right.map_err(debug)?;
    ensure(
        left.evidence_run_id == right.evidence_run_id,
        "concurrent matching retries must return the same durable evidence run",
    )?;
    ensure(
        serde_json::to_value(&left).map_err(debug)?
            == serde_json::to_value(&right).map_err(debug)?,
        "concurrent matching retries must replay the exact persisted result",
    )?;
    let run_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM matching.evidence_run WHERE registry_id=$1 AND organization_id=$2 AND query_source_record_id=$3::uuid",
    )
    .bind(&registry)
    .bind(&org)
    .bind(&query.source_record_id)
    .fetch_one(&pool)
    .await
    .map_err(debug)?;
    ensure(
        run_count == 1,
        "concurrent retries must persist exactly one run",
    )?;

    let replay = matcher
        .evaluate(
            &registry,
            &org,
            &query.source_record_id,
            20,
            "synthetic-matcher",
            "request-idempotency-replay",
            Some(idempotency),
        )
        .await
        .map_err(debug)?;
    ensure(
        serde_json::to_value(&left).map_err(debug)?
            == serde_json::to_value(&replay).map_err(debug)?,
        "a later retry must return the original persisted result unchanged",
    )?;
    let conflict = matcher
        .evaluate(
            &registry,
            &org,
            &query.source_record_id,
            21,
            "synthetic-matcher",
            "request-idempotency-conflict",
            Some(MatchIdempotency {
                request_digest: [0x44; 32],
                ..idempotency
            }),
        )
        .await;
    ensure(
        matches!(conflict, Err(MatchingStoreError::IdempotencyConflict)),
        "same key with different request parameters must return a stable conflict",
    )?;

    let different_actor = matcher
        .evaluate(
            &registry,
            &org,
            &query.source_record_id,
            20,
            "synthetic-matcher",
            "request-idempotency-other-actor",
            Some(MatchIdempotency {
                actor_digest: [0x55; 32],
                ..idempotency
            }),
        )
        .await
        .map_err(debug)?;
    ensure(
        different_actor.evidence_run_id != left.evidence_run_id,
        "the same key used by a different actor must have a separate run",
    )?;
    let different_org = matcher
        .evaluate(
            &registry,
            &other_org,
            &other_query.source_record_id,
            20,
            "synthetic-matcher",
            "request-idempotency-other-org",
            Some(idempotency),
        )
        .await
        .map_err(debug)?;
    ensure(
        different_org.evidence_run_id != left.evidence_run_id,
        "the same actor/key in a different organization must have a separate run",
    )?;
    let entries: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM matching.request_idempotency WHERE registry_id=$1",
    )
    .bind(&registry)
    .fetch_one(&pool)
    .await
    .map_err(debug)?;
    ensure(
        entries == 3,
        "only one original key, a different actor, and a different organization should persist",
    )?;
    pool.close().await;
    Ok(())
}

#[tokio::test]
async fn postgres_matching_index_preserves_authoritative_id_priority_at_limit() {
    let url = std::env::var("LITECR_TEST_DATABASE_URL")
        .expect("set LITECR_TEST_DATABASE_URL to the synthetic-only test DB");
    let key = suffix();
    let run = tokio::spawn(candidate_priority_scenario(url.clone(), key.clone())).await;
    cleanup(&url, &key).await;
    run.expect("candidate-priority scenario joined")
        .expect("candidate-priority database scenario");
}

#[tokio::test]
async fn postgres_matching_requires_explicit_original_name_paths_for_name_evidence() {
    let url = std::env::var("LITECR_TEST_DATABASE_URL")
        .expect("set LITECR_TEST_DATABASE_URL to the synthetic-only test DB");
    let key = suffix();
    let run = tokio::spawn(explicit_name_path_scenario(url.clone(), key.clone())).await;
    cleanup(&url, &key).await;
    run.expect("explicit-name-path scenario joined")
        .expect("explicit-name-path database scenario");
}

#[tokio::test]
async fn postgres_and_pure_matching_agree_on_multilingual_unicode_controls() {
    let url = std::env::var("LITECR_TEST_DATABASE_URL")
        .expect("set LITECR_TEST_DATABASE_URL to the synthetic-only test DB");
    let key = suffix();
    let run = tokio::spawn(multilingual_unicode_parity_scenario(
        url.clone(),
        key.clone(),
    ))
    .await;
    cleanup(&url, &key).await;
    run.expect("multilingual Unicode parity scenario joined")
        .expect("multilingual Unicode parity database scenario");
}

#[tokio::test]
async fn postgres_persists_twin_ambiguity_and_known_birth_day_conflict() {
    let url = std::env::var("LITECR_TEST_DATABASE_URL")
        .expect("set LITECR_TEST_DATABASE_URL to the synthetic-only test DB");
    let key = suffix();
    let run = tokio::spawn(twin_disambiguation_scenario(url.clone(), key.clone())).await;
    cleanup(&url, &key).await;
    run.expect("twin-disambiguation scenario joined")
        .expect("twin-disambiguation database scenario");
}

async fn twin_disambiguation_scenario(url: String, key: String) -> Result<(), String> {
    let pool = connect(&url, 8).await.map_err(debug)?;
    migrate(&pool).await.map_err(debug)?;
    let registry = format!("match-reg-{key}");
    let org = format!("match-org-{key}");
    let actor = RegistryActor {
        kind: ActorKind::System,
        id: format!("synthetic-match-seeder-{key}"),
        issuer_url: None,
        request_id: format!("twin-seed-{key}"),
        correlation_id: None,
    };
    let ingest = PostgresRegistryStore::new(pool.clone());
    let mut rows = Vec::new();
    for (label, date) in [
        ("query", "2001-02-03"),
        ("ambiguous", "2001-02-03"),
        ("different-day", "2001-02-04"),
    ] {
        rows.push(
            ingest
                .ingest(
                    &actor,
                    &format!("twin-{label}-{key}"),
                    &command(&registry, &org, &key, label, "ສີດາ ໄຊຍະວົງ", date),
                )
                .await
                .map_err(debug)?,
        );
    }
    let matcher = PostgresMatchingStore::new(pool);
    let result = matcher
        .evaluate(
            &registry,
            &org,
            &rows[0].source_record_id,
            10,
            "synthetic-matcher",
            "twin-safety-check",
            None,
        )
        .await
        .map_err(debug)?;
    let ambiguous = result
        .candidates
        .iter()
        .find(|c| c.candidate_record_id == rows[1].source_record_id)
        .ok_or("ambiguous candidate absent")?;
    ensure(
        ambiguous.outcome == litecr_matching::AdvisoryOutcome::InsufficientEvidence
            && ambiguous
                .fields
                .iter()
                .any(|f| f.rationale_code == "DEMOGRAPHICS_NOT_PERSON_SPECIFIC"),
        "persisted identical demographics must require person-specific evidence",
    )?;
    let different = result
        .candidates
        .iter()
        .find(|c| c.candidate_record_id == rows[2].source_record_id)
        .ok_or("different-day candidate absent")?;
    ensure(
        different.outcome == litecr_matching::AdvisoryOutcome::ConflictReview
            && different
                .hard_conflicts
                .contains(&"DOB_KNOWN_COMPONENT_CONFLICT".into()),
        "persisted known-day contradiction must remain visible as a conflict",
    )?;
    let saved = matcher
        .read_evidence(
            result
                .evidence_run_id
                .as_deref()
                .ok_or("evidence ID absent")?,
            &registry,
            &org,
        )
        .await
        .map_err(debug)?;
    ensure(
        saved == result,
        "historical evidence must retain the exact new safety classification",
    )?;
    Ok(())
}

async fn multilingual_unicode_parity_scenario(url: String, key: String) -> Result<(), String> {
    // These are constructed Unicode controls only. They test parity of
    // normalized candidate retrieval/evidence, not real-world match quality.
    let cases = [
        ("lo", "Laoo", "ກຸ້", "ກຸ້", "ກຸ"),
        ("en", "Latn", "  JOHN\tO'NEIL ", "john oneil", "john oneél"),
        ("th", "Thai", "กุ่", "กุ่", "กุ"),
        ("vi", "Latn", "Ánh", "A\u{0301}nh", "Anh"),
        ("hi-IN", "Deva", "क़लम", "क़लम", "कलम"),
    ];
    let pool = connect(&url, 8).await.map_err(debug)?;
    migrate(&pool).await.map_err(debug)?;
    let registry = format!("match-reg-{key}");
    let org = format!("match-org-{key}");
    let actor = RegistryActor {
        kind: ActorKind::System,
        id: format!("synthetic-match-seeder-{key}"),
        issuer_url: None,
        request_id: format!("multilingual-match-seed-{key}"),
        correlation_id: None,
    };
    let ingest = PostgresRegistryStore::new(pool.clone());
    let matcher = PostgresMatchingStore::new(pool.clone());

    for (index, &(language, script, query_name, equivalent_name, marked_neighbor)) in
        cases.iter().enumerate()
    {
        let names = [query_name, equivalent_name, marked_neighbor];
        let labels = ["query", "equivalent", "marked-neighbor"];
        let year = 1980 + index as i32;
        let dates = [
            format!("{year}-06"),
            format!("{year}-06-12"),
            format!("{year}-11-03"),
        ];
        let mut ingested = Vec::with_capacity(names.len());
        for ((name, label), dob) in names.into_iter().zip(labels).zip(&dates) {
            let mut source = command(
                &registry,
                &org,
                &key,
                &format!("p3-{index}-{label}"),
                name,
                dob,
            );
            source.assertions[0].language_tag = Some(language.into());
            source.assertions[0].script = Some(script.into());
            ingested.push(
                ingest
                    .ingest(&actor, &format!("seed-p3-{index}-{label}-{key}"), &source)
                    .await
                    .map_err(debug)?,
            );
        }

        let pure_records = names
            .iter()
            .zip(&ingested)
            .zip(&dates)
            .map(|((name, row), dob)| {
                let mut record =
                    match_record(&row.source_record_id, &registry, &org, name, dob, vec![]);
                record.name_assertions[0].language_tag = Some(language.into());
                record.name_assertions[0].script = Some(script.into());
                record
            })
            .collect::<Vec<_>>();
        let expected =
            litecr_matching::match_records(&ingested[0].source_record_id, &pure_records, 10)
                .map_err(debug)?;
        let actual = matcher
            .evaluate(
                &registry,
                &org,
                &ingested[0].source_record_id,
                10,
                "synthetic-matcher",
                &format!("request-p3-unicode-{index}-{key}"),
                None,
            )
            .await
            .map_err(debug)?;

        ensure(
            expected.candidates == actual.candidates,
            &format!(
                "{language}: indexed PostgreSQL candidates/evidence must equal the pure matcher"
            ),
        )?;
        ensure(
            expected.retrieval.per_pass_candidates == actual.retrieval.per_pass_candidates
                && expected.retrieval.union_before_limit == actual.retrieval.union_before_limit
                && expected.retrieval.truncated == actual.retrieval.truncated,
            &format!(
                "{language}: PostgreSQL candidate retrieval telemetry must equal pure matching"
            ),
        )?;
        ensure(
            expected.candidates.len() == 2,
            &format!("{language}: expected equivalent name plus partial-year mark control"),
        )?;
        let positive = expected
            .candidates
            .iter()
            .find(|candidate| candidate.candidate_record_id == ingested[1].source_record_id)
            .ok_or_else(|| format!("{language}: canonical-equivalent candidate missing"))?;
        ensure(
            positive.passes.contains(&litecr_matching::Pass::ExactName)
                && positive.fields.iter().any(|field| {
                    field.field == "name"
                        && field.state == litecr_matching::EvidenceState::Agreement
                }),
            &format!("{language}: canonical-equivalent name must be exact-name agreement"),
        )?;
        let negative = expected
            .candidates
            .iter()
            .find(|candidate| candidate.candidate_record_id == ingested[2].source_record_id)
            .ok_or_else(|| format!("{language}: significant-mark control missing"))?;
        ensure(
            negative
                .passes
                .contains(&litecr_matching::Pass::PartialBirthYear)
                && !negative.passes.contains(&litecr_matching::Pass::ExactName)
                && negative.fields.iter().any(|field| {
                    field.field == "name"
                        && field.points == 0
                        && matches!(
                            field.state,
                            litecr_matching::EvidenceState::Unknown
                                | litecr_matching::EvidenceState::CorrelatedWeak
                        )
                }),
            &format!("{language}: significant-mark neighbor must not become exact-name agreement"),
        )?;
    }
    pool.close().await;
    Ok(())
}

async fn explicit_name_path_scenario(url: String, key: String) -> Result<(), String> {
    let pool = connect(&url, 8).await.map_err(debug)?;
    migrate(&pool).await.map_err(debug)?;
    let registry = format!("match-reg-{key}");
    let org = format!("match-org-{key}");
    let actor = RegistryActor {
        kind: ActorKind::System,
        id: format!("synthetic-match-seeder-{key}"),
        issuer_url: None,
        request_id: format!("explicit-name-path-seed-{key}"),
        correlation_id: None,
    };
    let ingest = PostgresRegistryStore::new(pool.clone());

    let query = ingest
        .ingest(
            &actor,
            &format!("seed-query-{key}"),
            &command(&registry, &org, &key, "query", "Same Person", "1986-04"),
        )
        .await
        .map_err(debug)?;

    let mut transliteration = command(
        &registry,
        &org,
        &key,
        "transliteration",
        "Same Person",
        "1986-04-19",
    );
    transliteration.assertions[0].field_path = "name.transliteration".into();
    let transliteration = ingest
        .ingest(
            &actor,
            &format!("seed-transliteration-{key}"),
            &transliteration,
        )
        .await
        .map_err(debug)?;

    let mut serialized_fhir = command(
        &registry,
        &org,
        &key,
        "fhir-name",
        "Same Person",
        "1986-04-19",
    );
    serialized_fhir.assertions[0].field_path = "fhir.name".into();
    let serialized_fhir = ingest
        .ingest(&actor, &format!("seed-fhir-name-{key}"), &serialized_fhir)
        .await
        .map_err(debug)?;

    let original = ingest
        .ingest(
            &actor,
            &format!("seed-original-{key}"),
            &command(
                &registry,
                &org,
                &key,
                "original",
                "Same Person",
                "1986-04-19",
            ),
        )
        .await
        .map_err(debug)?;

    let unclassified_name_index_rows: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM matching.normalized_assertion WHERE source_record_id IN ($1::uuid,$2::uuid) AND lower(field_path) IN ('name.transliteration','fhir.name')",
    )
    .bind(&transliteration.source_record_id)
    .bind(&serialized_fhir.source_record_id)
    .fetch_one(&pool)
    .await
    .map_err(debug)?;
    ensure(
        unclassified_name_index_rows == 0,
        "unclassified name-like assertions must be retained only as source provenance, not indexed as name keys",
    )?;
    let retained_name_path_rows: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM litecr.source_assertion WHERE source_record_id IN ($1::uuid,$2::uuid) AND field_path IN ('name.transliteration','fhir.name')",
    )
    .bind(&transliteration.source_record_id)
    .bind(&serialized_fhir.source_record_id)
    .fetch_one(&pool)
    .await
    .map_err(debug)?;
    ensure(
        retained_name_path_rows == 2,
        "unclassified name-like assertions must remain in immutable source history",
    )?;

    let result = PostgresMatchingStore::new(pool.clone())
        .evaluate(
            &registry,
            &org,
            &query.source_record_id,
            50,
            "synthetic-matcher",
            "request-explicit-name-paths",
            None,
        )
        .await
        .map_err(debug)?;

    ensure(
        result.candidates.len() == 3,
        "DOB should retrieve all three synthetic candidates",
    )?;
    let candidate = |source_record_id: &str| {
        result
            .candidates
            .iter()
            .find(|item| item.candidate_record_id == source_record_id)
    };
    for source_record_id in [
        &transliteration.source_record_id,
        &serialized_fhir.source_record_id,
    ] {
        let candidate = candidate(source_record_id)
            .ok_or_else(|| format!("DOB candidate {source_record_id} was not retrieved"))?;
        ensure(
            candidate
                .passes
                .contains(&litecr_matching::Pass::PartialBirthYear)
                && !candidate.passes.contains(&litecr_matching::Pass::ExactName),
            "unclassified name-like paths must not enter exact-name retrieval",
        )?;
        let name = candidate
            .fields
            .iter()
            .find(|field| field.field == "name")
            .ok_or_else(|| "candidate is missing name evidence".to_owned())?;
        ensure(
            name.state == litecr_matching::EvidenceState::Unknown,
            "unclassified name-like paths must not produce name agreement",
        )?;
    }

    let original_candidate = candidate(&original.source_record_id)
        .ok_or_else(|| "explicit name.original candidate was not retrieved".to_owned())?;
    ensure(
        original_candidate
            .passes
            .contains(&litecr_matching::Pass::ExactName),
        "explicit name.original path should preserve exact-name retrieval",
    )?;
    let name = original_candidate
        .fields
        .iter()
        .find(|field| field.field == "name")
        .ok_or_else(|| "original candidate is missing name evidence".to_owned())?;
    ensure(
        name.state == litecr_matching::EvidenceState::Agreement,
        "explicit name.original path should preserve exact-name agreement",
    )?;

    pool.close().await;
    Ok(())
}

async fn candidate_priority_scenario(url: String, key: String) -> Result<(), String> {
    let pool = connect(&url, 8).await.map_err(debug)?;
    migrate(&pool).await.map_err(debug)?;
    let registry = format!("match-reg-{key}");
    let org = format!("match-org-{key}");
    let actor = RegistryActor {
        kind: ActorKind::System,
        id: format!("synthetic-match-seeder-{key}"),
        issuer_url: None,
        request_id: format!("candidate-priority-seed-{key}"),
        correlation_id: None,
    };
    let ingest = PostgresRegistryStore::new(pool.clone());
    let mut query_command = command(&registry, &org, &key, "query", "Query Person", "1986-04");
    query_command
        .identifiers
        .push(litecr_domain::registry::IdentifierAssertion {
            authority: "national".into(),
            identifier_type: "person".into(),
            value: "ID-001".into(),
            authoritative: true,
        });
    let query = ingest
        .ingest(&actor, &format!("seed-query-{key}"), &query_command)
        .await
        .map_err(debug)?;
    let authoritative = ingest
        .ingest(
            &actor,
            &format!("seed-authoritative-{key}"),
            &command(
                &registry,
                &org,
                &key,
                "authoritative",
                "Other Person",
                "1970-01-01",
            ),
        )
        .await
        .map_err(debug)?;
    let mut normalized_collision_command = command(
        &registry,
        &org,
        &key,
        "identifier-variant",
        "Unrelated Person",
        "1973-06-15",
    );
    normalized_collision_command
        .identifiers
        .push(litecr_domain::registry::IdentifierAssertion {
            authority: "national".into(),
            identifier_type: "person".into(),
            value: "ID001".into(),
            authoritative: true,
        });
    let normalized_collision = ingest
        .ingest(
            &actor,
            &format!("seed-identifier-variant-{key}"),
            &normalized_collision_command,
        )
        .await
        .map_err(debug)?;
    let mut weaker_ids = Vec::new();
    for idx in 0..5 {
        let weaker = ingest
            .ingest(
                &actor,
                &format!("seed-weaker-{idx}-{key}"),
                &command(
                    &registry,
                    &org,
                    &key,
                    &format!("weaker-{idx}"),
                    &format!("Person {idx}"),
                    "1986-11-11",
                ),
            )
            .await
            .map_err(debug)?;
        weaker_ids.push(weaker.source_record_id);
    }
    sqlx::query("INSERT INTO litecr.source_identifier_assertion(source_record_id,receive_version,registry_id,organization_id,authority,identifier_type,original_value,authoritative,assertion_state) VALUES($1::uuid,1,$2,$3,'national','person','ID-001',TRUE,'accepted')")
        .bind(&authoritative.source_record_id).bind(&registry).bind(&org)
        .execute(&pool).await.map_err(debug)?;
    let id_key = litecr_matching::authoritative_identifier_key("ID-001")
        .expect("non-empty authoritative ID has an index key");
    sqlx::query("INSERT INTO matching.normalized_identifier(source_record_id,registry_id,organization_id,receive_version,authority,identifier_type,normalizer_version,normalized_key) VALUES($1::uuid,$2,$3,1,'national','person',$4,$5)")
        .bind(&authoritative.source_record_id).bind(&registry).bind(&org)
        .bind(litecr_matching::NORMALIZATION_VERSION).bind(id_key)
        .execute(&pool).await.map_err(debug)?;

    let result = PostgresMatchingStore::new(pool.clone())
        .evaluate(
            &registry,
            &org,
            &query.source_record_id,
            1,
            "synthetic-matcher",
            "request-id-priority",
            None,
        )
        .await
        .map_err(debug)?;
    ensure(
        result.retrieval.union_before_limit == 6,
        "retrieval must see the exact authoritative hit and five weaker birth-year hits, but not a punctuation-folded ID variant",
    )?;
    ensure(
        result
            .retrieval
            .per_pass_candidates
            .get(&litecr_matching::Pass::AuthoritativeIdentifier)
            == Some(&1),
        "exact-ID retrieval must exclude a candidate that only matches after name normalization",
    )?;
    ensure(
        !result.candidates.iter().any(|candidate| {
            candidate.candidate_record_id == normalized_collision.source_record_id
        }),
        "punctuation-folded identifier variant must not appear as an authoritative-ID candidate",
    )?;

    // A prior v2 index marker is not trusted as an exact-key index. It must
    // remain discoverable through exhaustive fallback until the v3 reindex
    // replaces the old marker, while the pure matcher still rejects its ID.
    for table in [
        "matching.normalized_assertion",
        "matching.normalized_identifier",
        "matching.indexed_source_version",
    ] {
        let query = format!(
            "UPDATE {table} SET normalizer_version='unicode-nfc-case-unicode-whitespace-ascii-punctuation-fold-v2' WHERE source_record_id=$1::uuid"
        );
        sqlx::query(&query)
            .bind(&normalized_collision.source_record_id)
            .execute(&pool)
            .await
            .map_err(debug)?;
    }
    let legacy_index = PostgresMatchingStore::new(pool.clone())
        .evaluate(
            &registry,
            &org,
            &query.source_record_id,
            100,
            "synthetic-matcher",
            "request-legacy-id-index-fallback",
            None,
        )
        .await
        .map_err(debug)?;
    ensure(
        !legacy_index.candidates.iter().any(|candidate| {
            candidate.candidate_record_id == normalized_collision.source_record_id
        }),
        "legacy v2 fallback must not turn a normalized ID collision into a match",
    )?;
    let reindexed = PostgresMatchingStore::new(pool.clone())
        .reindex_next_batch(Some(&registry), Some(&org), 10)
        .await
        .map_err(debug)?;
    ensure(
        reindexed == 1,
        "bounded reindex must replace the legacy version marker with v3 exact keys",
    )?;
    ensure(
        result.candidates.len() == 1,
        "candidate limit must remain enforced",
    )?;
    ensure(
        result.candidates[0].candidate_record_id == authoritative.source_record_id,
        "authoritative-ID hit must rank ahead of weaker birth-year-only candidates",
    )?;
    ensure(
        result.candidates[0]
            .passes
            .contains(&litecr_matching::Pass::AuthoritativeIdentifier),
        "authoritative-ID pass must remain in persisted candidate evidence",
    )?;
    let mut exhaustive = vec![match_record(
        &query.source_record_id,
        &registry,
        &org,
        "Query Person",
        "1986-04",
        vec![litecr_matching::MatchIdentifier {
            authority: "national".into(),
            identifier_type: "person".into(),
            value: "ID-001".into(),
            authoritative: true,
            status: Some("accepted".into()),
        }],
    )];
    exhaustive.push(match_record(
        &authoritative.source_record_id,
        &registry,
        &org,
        "Other Person",
        "1970-01-01",
        vec![litecr_matching::MatchIdentifier {
            authority: "national".into(),
            identifier_type: "person".into(),
            value: "ID-001".into(),
            authoritative: true,
            status: Some("accepted".into()),
        }],
    ));
    for (idx, record_id) in weaker_ids.iter().enumerate() {
        exhaustive.push(match_record(
            record_id,
            &registry,
            &org,
            &format!("Person {idx}"),
            "1986-11-11",
            vec![],
        ));
    }
    let exhaustive_result =
        litecr_matching::match_records(&query.source_record_id, &exhaustive, 1).map_err(debug)?;
    ensure(
        exhaustive_result.retrieval.union_before_limit == result.retrieval.union_before_limit
            && exhaustive_result.retrieval.per_pass_candidates
                == result.retrieval.per_pass_candidates
            && exhaustive_result.candidates == result.candidates,
        "indexed retrieval and the prior exhaustive matcher must produce identical candidates, pass counts, and limit order on the deterministic fixture",
    )?;
    pool.close().await;
    Ok(())
}

fn match_record(
    record_id: &str,
    registry_id: &str,
    organization_id: &str,
    name: &str,
    birth_date: &str,
    identifiers: Vec<litecr_matching::MatchIdentifier>,
) -> litecr_matching::MatchRecord {
    let assertion = |field_path: &str, original_value: &str| litecr_matching::MatchAssertion {
        field_path: field_path.into(),
        original_value: original_value.into(),
        language_tag: None,
        script: None,
        representation: None,
        normalization_version: None,
    };
    litecr_matching::MatchRecord {
        record_id: record_id.into(),
        registry_id: registry_id.into(),
        organization_id: organization_id.into(),
        source_version: Some("opaque-v1".into()),
        receive_version: Some(1),
        name_assertions: vec![assertion("name", name)],
        assertions: vec![assertion("name", name), assertion("birth_date", birth_date)],
        date_of_birth: None,
        identifiers,
        contacts: vec![],
        address_token: None,
    }
}

async fn stale_snapshot_scenario(url: String, key: String) -> Result<(), String> {
    let pool = connect(&url, 8).await.map_err(debug)?;
    migrate(&pool).await.map_err(debug)?;
    let registry = format!("match-reg-{key}");
    let org = format!("match-org-{key}");
    let actor = RegistryActor {
        kind: ActorKind::System,
        id: format!("synthetic-match-seeder-{key}"),
        issuer_url: None,
        request_id: format!("stale-match-seed-{key}"),
        correlation_id: None,
    };
    let ingest = PostgresRegistryStore::new(pool.clone());
    let matcher = PostgresMatchingStore::new(pool.clone());

    // Hold each source row until the matching transaction reaches its final
    // version check. Updating it before releasing the lock makes the race
    // deterministic without adding a production test hook.
    for case in ["query", "candidate"] {
        let query_record = format!("{case}-query");
        let candidate_record = format!("{case}-candidate");
        let query = ingest
            .ingest(
                &actor,
                &format!("stale-seed-query-{case}-{key}"),
                &command(&registry, &org, &key, &query_record, "ສົມໃຈ", "1986-04"),
            )
            .await
            .map_err(debug)?;
        let candidate = ingest
            .ingest(
                &actor,
                &format!("stale-seed-candidate-{case}-{key}"),
                &command(
                    &registry,
                    &org,
                    &key,
                    &candidate_record,
                    "ສົມໃຈ",
                    "1986-04-19",
                ),
            )
            .await
            .map_err(debug)?;
        let changed_source_id = if case == "query" {
            query.source_record_id.as_str()
        } else {
            candidate.source_record_id.as_str()
        };
        let mut lock_tx = pool.begin().await.map_err(debug)?;
        let _: String = sqlx::query_scalar(
            "SELECT source_record_id::text FROM litecr.source_record \
             WHERE source_record_id=$1::uuid FOR UPDATE",
        )
        .bind(changed_source_id)
        .fetch_one(&mut *lock_tx)
        .await
        .map_err(debug)?;
        let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut *lock_tx)
            .await
            .map_err(debug)?;

        let matching = matcher.clone();
        let registry_for_match = registry.clone();
        let org_for_match = org.clone();
        let query_id = query.source_record_id.clone();
        let request_id = format!("stale-match-{case}-{key}");
        let matching_task = tokio::spawn(async move {
            matching
                .evaluate(
                    &registry_for_match,
                    &org_for_match,
                    &query_id,
                    50,
                    "synthetic-matcher",
                    &request_id,
                    None,
                )
                .await
        });

        let matching_task = wait_for_matching_lock_wait(&pool, blocker_pid, matching_task).await?;
        advance_source_receive_version(&mut lock_tx, changed_source_id)
            .await
            .map_err(debug)?;
        lock_tx.commit().await.map_err(debug)?;

        let result = timeout(Duration::from_secs(5), matching_task)
            .await
            .map_err(|_| format!("{case} stale matching call timed out"))?
            .map_err(debug)?;
        ensure(
            matches!(result, Err(MatchingStoreError::StaleSnapshot)),
            &format!("changed {case} source must reject stale evidence: {result:?}"),
        )?;

        let evidence_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM matching.evidence_run \
             WHERE registry_id=$1 AND organization_id=$2 AND query_source_record_id=$3::uuid",
        )
        .bind(&registry)
        .bind(&org)
        .bind(&query.source_record_id)
        .fetch_one(&pool)
        .await
        .map_err(debug)?;
        ensure(
            evidence_count == 0,
            &format!("changed {case} source must not insert evidence"),
        )?;
    }

    pool.close().await;
    Ok(())
}

async fn wait_for_matching_lock_wait(
    pool: &PgPool,
    blocker_pid: i32,
    matching_task: tokio::task::JoinHandle<
        Result<litecr_matching::MatchResult, MatchingStoreError>,
    >,
) -> Result<tokio::task::JoinHandle<Result<litecr_matching::MatchResult, MatchingStoreError>>, String>
{
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut matching_task = matching_task;
    loop {
        if matching_task.is_finished() {
            let result = (&mut matching_task).await.map_err(debug)?;
            return Err(format!(
                "matching finished before reaching the persistence version lock: {result:?}"
            ));
        }
        let blocked: bool = sqlx::query_scalar(
            "SELECT EXISTS (\
               SELECT 1 FROM pg_stat_activity a \
               WHERE $1 = ANY(pg_blocking_pids(a.pid)) \
                 AND a.wait_event_type = 'Lock')",
        )
        .bind(blocker_pid)
        .fetch_one(pool)
        .await
        .map_err(debug)?;
        if blocked {
            return Ok(matching_task);
        }
        if Instant::now() >= deadline {
            let observed: String = sqlx::query_scalar(
                "SELECT COALESCE(string_agg(pid::text || ':' || COALESCE(wait_event_type,'') || ':' || \
                   COALESCE(wait_event,'') || ':' || left(query,160), ' | '), 'no other activity') \
                 FROM pg_stat_activity WHERE datname=current_database() AND pid <> pg_backend_pid()",
            )
            .fetch_one(pool)
            .await
            .map_err(debug)?;
            return Err(format!(
                "matching store did not reach the persistence version lock; observed={observed}"
            ));
        }
        sleep(Duration::from_millis(10)).await;
    }
}

async fn advance_source_receive_version(
    tx: &mut Transaction<'_, Postgres>,
    source_record_id: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO litecr.source_record_version \
         (source_record_id, receive_version, source_version, payload_hash, version_state) \
         SELECT sr.source_record_id, sr.current_receive_version + 1, 'opaque-v2', sv.payload_hash, 'accepted' \
         FROM litecr.source_record sr \
         JOIN litecr.source_record_version sv \
           ON sv.source_record_id=sr.source_record_id \
          AND sv.receive_version=sr.current_receive_version \
         WHERE sr.source_record_id=$1::uuid",
    )
    .bind(source_record_id)
    .execute(&mut **tx)
    .await?;
    sqlx::query(
        "UPDATE litecr.source_record \
         SET current_receive_version=current_receive_version + 1 \
         WHERE source_record_id=$1::uuid",
    )
    .bind(source_record_id)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn scenario(url: String, key: String) -> Result<(), String> {
    let pool = connect(&url, 5).await.map_err(debug)?;
    migrate(&pool).await.map_err(debug)?;
    let registry = format!("match-reg-{key}");
    let org = format!("match-org-{key}");
    let other_org = format!("match-other-{key}");
    let actor = RegistryActor {
        kind: ActorKind::System,
        id: format!("synthetic-match-seeder-{key}"),
        issuer_url: None,
        request_id: format!("match-seed-{key}"),
        correlation_id: None,
    };
    let ingest = PostgresRegistryStore::new(pool.clone());
    let query = ingest
        .ingest(
            &actor,
            &format!("seed-query-{key}"),
            &command(&registry, &org, &key, "query", "ສົມໃຈ", "1986-04"),
        )
        .await
        .map_err(debug)?;
    let candidate = ingest
        .ingest(
            &actor,
            &format!("seed-candidate-{key}"),
            &command(&registry, &org, &key, "candidate", "ສົມໃຈ", "1986-04-19"),
        )
        .await
        .map_err(debug)?;
    ingest
        .ingest(
            &actor,
            &format!("seed-other-org-{key}"),
            &command(&registry, &other_org, &key, "foreign", "ສົມໃຈ", "1986-04-20"),
        )
        .await
        .map_err(debug)?;
    let matcher = PostgresMatchingStore::new(pool.clone());
    let indexed_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM matching.indexed_source_version WHERE source_record_id IN ($1::uuid,$2::uuid) AND normalizer_version=$3",
    )
    .bind(&query.source_record_id)
    .bind(&candidate.source_record_id)
    .bind(litecr_matching::NORMALIZATION_VERSION)
    .fetch_one(&pool)
    .await
    .map_err(debug)?;
    ensure(
        indexed_count == 2,
        "accepted query and candidate versions must be indexed transactionally",
    )?;
    let result = matcher
        .evaluate(
            &registry,
            &org,
            &query.source_record_id,
            50,
            "synthetic-matcher",
            "request-matching-synthetic",
            None,
        )
        .await
        .map_err(debug)?;
    ensure(
        result.query_record_id == query.source_record_id,
        "query source ID must be bound to the response",
    )?;
    ensure(
        result.query_receive_version == Some(query.source_receive_version),
        "query receive version must be present",
    )?;
    ensure(
        result.candidates.len() == 1,
        "only the in-organization identity candidate should be returned",
    )?;
    ensure(
        result.candidates[0].candidate_receive_version == Some(1),
        "candidate receive version must be included",
    )?;
    ensure(
        result.candidates[0].candidate_source_version.as_deref() == Some("opaque-v1"),
        "candidate source version must be included",
    )?;
    ensure(
        result.candidates[0]
            .passes
            .contains(&litecr_matching::Pass::ExactName),
        "exact-name pass should be explained",
    )?;
    ensure(
        result.candidates[0]
            .passes
            .contains(&litecr_matching::Pass::PartialBirthYear),
        "partial-date pass should be explained",
    )?;
    ensure(
        result.evidence_run_id.is_some() && result.snapshot_complete,
        "new match output must be bound to a persisted complete snapshot",
    )?;

    // Simulate a schema-upgrade legacy row: it must remain visible through the
    // exhaustive fallback until the bounded, repeatable reindex command covers it.
    sqlx::query("DELETE FROM matching.normalized_assertion WHERE source_record_id=$1::uuid")
        .bind(&candidate.source_record_id)
        .execute(&pool)
        .await
        .map_err(debug)?;
    sqlx::query("DELETE FROM matching.indexed_source_version WHERE source_record_id=$1::uuid")
        .bind(&candidate.source_record_id)
        .execute(&pool)
        .await
        .map_err(debug)?;
    sqlx::query("DELETE FROM matching.normalized_assertion WHERE source_record_id=$1::uuid")
        .bind(&query.source_record_id)
        .execute(&pool)
        .await
        .map_err(debug)?;
    sqlx::query("DELETE FROM matching.indexed_source_version WHERE source_record_id=$1::uuid")
        .bind(&query.source_record_id)
        .execute(&pool)
        .await
        .map_err(debug)?;
    let fallback = matcher
        .evaluate(
            &registry,
            &org,
            &query.source_record_id,
            50,
            "synthetic-matcher",
            "request-legacy-fallback",
            None,
        )
        .await
        .map_err(debug)?;
    ensure(
        fallback.candidates.len() == 1,
        "unindexed current version must remain discoverable through the compatibility fallback",
    )?;
    let scoped_pending: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM litecr.source_record sr \
         JOIN litecr.source_record_version sv \
           ON sv.source_record_id=sr.source_record_id AND sv.receive_version=sr.current_receive_version \
         WHERE sr.source_record_id IN ($1::uuid,$2::uuid) \
           AND sr.withdrawn_at IS NULL AND sv.withdrawn_at IS NULL AND sv.version_state='accepted' \
           AND NOT EXISTS (SELECT 1 FROM matching.indexed_source_version ix \
             WHERE ix.source_record_id=sr.source_record_id AND ix.receive_version=sr.current_receive_version \
               AND ix.normalizer_version=$3)",
    )
    .bind(&candidate.source_record_id)
    .bind(&query.source_record_id)
    .bind(litecr_matching::NORMALIZATION_VERSION)
    .fetch_one(&pool)
    .await
    .map_err(debug)?;
    ensure(
        scoped_pending == 2
            && matcher.pending_reindex_count().await.map_err(debug)? >= scoped_pending,
        "doctor diagnostic must include current accepted versions missing the active index",
    )?;
    let reindexed = matcher
        .reindex_next_batch(Some(&registry), Some(&org), 10)
        .await
        .map_err(debug)?;
    ensure(
        reindexed == 2,
        "bounded backfill must index the two unindexed accepted source versions",
    )?;
    let backfill_repeat = matcher
        .reindex_next_batch(Some(&registry), Some(&org), 10)
        .await
        .map_err(debug)?;
    ensure(
        backfill_repeat == 0,
        "re-running a completed backfill batch must be idempotent",
    )?;
    let scoped_pending_after: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM litecr.source_record sr \
         JOIN litecr.source_record_version sv \
           ON sv.source_record_id=sr.source_record_id AND sv.receive_version=sr.current_receive_version \
         WHERE sr.source_record_id IN ($1::uuid,$2::uuid) \
           AND sr.withdrawn_at IS NULL AND sv.withdrawn_at IS NULL AND sv.version_state='accepted' \
           AND NOT EXISTS (SELECT 1 FROM matching.indexed_source_version ix \
             WHERE ix.source_record_id=sr.source_record_id AND ix.receive_version=sr.current_receive_version \
               AND ix.normalizer_version=$3)",
    )
    .bind(&candidate.source_record_id)
    .bind(&query.source_record_id)
    .bind(litecr_matching::NORMALIZATION_VERSION)
    .fetch_one(&pool)
    .await
    .map_err(debug)?;
    ensure(
        scoped_pending_after == 0,
        "completed bounded backfill must clear this scenario's pending-index diagnostic",
    )?;
    let after_reindex = matcher
        .evaluate(
            &registry,
            &org,
            &query.source_record_id,
            50,
            "synthetic-matcher",
            "request-indexed-hit",
            None,
        )
        .await
        .map_err(debug)?;
    ensure(
        after_reindex.candidates.len() == 1,
        "indexed name/year hit must equal the legacy fallback result",
    )?;

    // A later accepted source version and a withdrawal must not keep matching
    // against old indexed keys from the previous receive version.
    let mut changed = command(
        &registry,
        &org,
        &key,
        "candidate",
        "Different Name",
        "1991-02-03",
    );
    changed.source_version = Some("opaque-v2".into());
    ingest
        .ingest(&actor, &format!("update-candidate-{key}"), &changed)
        .await
        .map_err(debug)?;
    let after_update = matcher
        .evaluate(
            &registry,
            &org,
            &query.source_record_id,
            50,
            "synthetic-matcher",
            "request-after-update",
            None,
        )
        .await
        .map_err(debug)?;
    ensure(
        after_update.candidates.is_empty(),
        "old version keys must not survive candidate retrieval after an accepted update",
    )?;
    sqlx::query(
        "UPDATE litecr.source_record SET withdrawn_at=now(),withdrawn_by_manifest=decode(repeat('00',32),'hex') WHERE source_record_id=$1::uuid",
    )
    .bind(&candidate.source_record_id)
    .execute(&pool)
    .await
    .map_err(debug)?;
    let after_withdrawal = matcher
        .evaluate(
            &registry,
            &org,
            &query.source_record_id,
            50,
            "synthetic-matcher",
            "request-after-withdrawal",
            None,
        )
        .await
        .map_err(debug)?;
    ensure(
        after_withdrawal.candidates.is_empty(),
        "withdrawn records must be excluded even when historical index rows remain",
    )?;
    let persisted = matcher
        .read_evidence(
            result
                .evidence_run_id
                .as_deref()
                .ok_or("missing evidence run ID")?,
            &registry,
            &org,
        )
        .await
        .map_err(debug)?;
    ensure(
        persisted == result,
        "review evidence read must return the original stored MatchResult unchanged after source mutations",
    )?;
    sqlx::query("UPDATE matching.evidence_run SET result_snapshot=NULL WHERE run_id=$1::uuid")
        .bind(
            result
                .evidence_run_id
                .as_deref()
                .ok_or("missing evidence run ID")?,
        )
        .execute(&pool)
        .await
        .map_err(debug)?;
    let legacy = matcher
        .read_evidence(
            result
                .evidence_run_id
                .as_deref()
                .ok_or("missing evidence run ID")?,
            &registry,
            &org,
        )
        .await
        .map_err(debug)?;
    ensure(
        !legacy.snapshot_complete
            && !legacy.retrieval.complete
            && legacy.candidates == result.candidates
            && legacy.versions == result.versions,
        "legacy evidence reconstruction must retain saved candidates and versions while explicitly marking telemetry incomplete",
    )?;
    ensure(
        matches!(
            matcher
                .read_evidence(
                    result
                        .evidence_run_id
                        .as_deref()
                        .ok_or("missing evidence run ID")?,
                    &registry,
                    &other_org,
                )
                .await,
            Err(MatchingStoreError::EvidenceNotFound)
        ),
        "cross-organization evidence IDs must be indistinguishable from missing IDs",
    )?;
    ensure(
        matches!(
            matcher
                .read_evidence("00000000-0000-0000-0000-000000000000", &registry, &org)
                .await,
            Err(MatchingStoreError::EvidenceNotFound)
        ),
        "unknown evidence IDs must return the same not-found result",
    )?;
    let evidence=sqlx::query("SELECT er.actor_id,er.request_id,er.query_receive_version,er.candidate_count,ec.candidate_receive_version,ec.candidate_source_version,ec.score_type FROM matching.evidence_run er JOIN matching.evidence_candidate ec USING(run_id) WHERE er.registry_id=$1 AND er.organization_id=$2 AND er.query_source_record_id=$3::uuid AND er.request_id='request-matching-synthetic'")
        .bind(&registry).bind(&org).bind(&query.source_record_id).fetch_one(&pool).await.map_err(debug)?;
    let actor_id: String = evidence.try_get("actor_id").map_err(debug)?;
    let req: String = evidence.try_get("request_id").map_err(debug)?;
    let version: i64 = evidence.try_get("query_receive_version").map_err(debug)?;
    let count: i32 = evidence.try_get("candidate_count").map_err(debug)?;
    let candidate_version: i64 = evidence
        .try_get("candidate_receive_version")
        .map_err(debug)?;
    let source_version: String = evidence
        .try_get("candidate_source_version")
        .map_err(debug)?;
    let score_type: String = evidence.try_get("score_type").map_err(debug)?;
    ensure(
        actor_id == "synthetic-matcher" && req == "request-matching-synthetic",
        "evidence run should preserve request actor lineage",
    )?;
    ensure(
        version == 1 && candidate_version == 1 && count == 1,
        "evidence should retain both receive versions and bounded candidate count",
    )?;
    ensure(
        source_version == "opaque-v1" && score_type == "raw_heuristic_points_not_probability",
        "evidence should retain opaque source version and score type",
    )?;
    let denied = matcher
        .evaluate(
            &registry,
            &other_org,
            &query.source_record_id,
            50,
            "synthetic-matcher",
            "request-cross-scope",
            None,
        )
        .await;
    ensure(
        denied.is_err(),
        "a query record must not resolve in another organization",
    )?;
    pool.close().await;
    Ok(())
}

fn command(
    registry: &str,
    org: &str,
    key: &str,
    record: &str,
    name: &str,
    dob: &str,
) -> SourceRecordCommand {
    SourceRecordCommand {
        registry_id: registry.into(),
        organization_id: org.into(),
        source_system_id: format!("match-source-{key}"),
        source_record_key: format!("match-record-{record}-{key}"),
        source_version: Some("opaque-v1".into()),
        assertions: vec![
            SourceAssertion {
                field_path: "name.original".into(),
                original_value: name.into(),
                language_tag: Some("lo".into()),
                script: Some("Lao".into()),
            },
            SourceAssertion {
                field_path: "birth_date".into(),
                original_value: dob.into(),
                language_tag: None,
                script: None,
            },
        ],
        identifiers: vec![],
    }
}
async fn cleanup(url: &str, key: &str) {
    let pool = connect(url, 2)
        .await
        .expect("synthetic matching database available for cleanup");
    let registry = format!("match-reg-{key}");
    let orgs = [format!("match-org-{key}"), format!("match-other-{key}")];
    let mut tx = pool.begin().await.expect("matching cleanup tx");
    // Idempotency stores source_record_id as a foreign key; remove this test actor's rows first.
    sqlx::query("DELETE FROM litecr.registry_idempotency WHERE actor_kind='system' AND actor_issuer_url='' AND actor_id=$1 AND registry_id=$2")
        .bind(format!("synthetic-match-seeder-{key}"))
        .bind(&registry)
        .execute(&mut *tx)
        .await
        .expect("delete only this synthetic matching actor's idempotency rows before source records");
    for org in &orgs {
        sqlx::query(
            "DELETE FROM litecr.consumer_receipt r USING litecr.consumer_job j, litecr.outbox_event o \
             WHERE r.consumer_name=j.consumer_name AND r.event_id=j.event_id AND o.event_id=j.event_id \
               AND o.registry_id=$1 AND o.organization_id=$2 AND o.actor_kind='system' AND o.actor_id=$3",
        )
        .bind(&registry)
        .bind(org)
        .bind(format!("synthetic-match-seeder-{key}"))
        .execute(&mut *tx)
        .await
        .expect("delete this synthetic matching actor's exact-scope worker receipts");
        sqlx::query(
            "DELETE FROM litecr.consumer_job j USING litecr.outbox_event o \
             WHERE o.event_id=j.event_id AND o.registry_id=$1 AND o.organization_id=$2 \
               AND o.actor_kind='system' AND o.actor_id=$3",
        )
        .bind(&registry)
        .bind(org)
        .bind(format!("synthetic-match-seeder-{key}"))
        .execute(&mut *tx)
        .await
        .expect("delete this synthetic matching actor's exact-scope worker jobs");
        sqlx::query("DELETE FROM litecr.outbox_event WHERE registry_id=$1 AND organization_id=$2 AND actor_kind='system' AND actor_id=$3")
        .bind(&registry)
        .bind(org)
        .bind(format!("synthetic-match-seeder-{key}"))
        .execute(&mut *tx)
        .await
        .expect("delete only this synthetic matching actor's scoped outbox rows");
        sqlx::query("DELETE FROM matching.evidence_candidate WHERE run_id IN (SELECT run_id FROM matching.evidence_run WHERE registry_id=$1 AND organization_id=$2)").bind(&registry).bind(org).execute(&mut *tx).await.expect("delete this test's exact-scope matching candidate evidence");
        sqlx::query(
            "DELETE FROM matching.request_idempotency WHERE registry_id=$1 AND organization_id=$2",
        )
        .bind(&registry)
        .bind(org)
        .execute(&mut *tx)
        .await
        .expect("delete this test's exact-scope matching idempotency rows");
        sqlx::query(
            "DELETE FROM matching.evidence_run WHERE registry_id=$1 AND organization_id=$2",
        )
        .bind(&registry)
        .bind(org)
        .execute(&mut *tx)
        .await
        .expect("delete this test's exact-scope matching run evidence");
        sqlx::query(
            "DELETE FROM matching.normalized_assertion WHERE registry_id=$1 AND organization_id=$2",
        )
        .bind(&registry)
        .bind(org)
        .execute(&mut *tx)
        .await
        .expect("delete exact-scope derived index rows before source assertions");
        sqlx::query("DELETE FROM matching.normalized_identifier WHERE registry_id=$1 AND organization_id=$2")
            .bind(&registry).bind(org).execute(&mut *tx).await
            .expect("delete exact-scope normalized identifiers before source versions");
        sqlx::query("DELETE FROM matching.indexed_source_version WHERE source_record_id IN (SELECT source_record_id FROM litecr.source_record WHERE registry_id=$1 AND organization_id=$2)")
            .bind(&registry).bind(org).execute(&mut *tx).await
            .expect("delete exact-scope index markers before source versions");
        sqlx::query("DELETE FROM litecr.source_identifier_assertion WHERE registry_id=$1 AND organization_id=$2").bind(&registry).bind(org).execute(&mut *tx).await.expect("delete exact-scope identifier assertions");
        sqlx::query("DELETE FROM matching.normalized_assertion WHERE source_record_id IN (SELECT source_record_id FROM litecr.source_record WHERE registry_id=$1 AND organization_id=$2)")
            .bind(&registry).bind(org).execute(&mut *tx).await
            .expect("delete exact-scope normalized assertions");
        sqlx::query(
            "DELETE FROM litecr.person_identifier WHERE registry_id=$1 AND organization_id=$2",
        )
        .bind(&registry)
        .bind(org)
        .execute(&mut *tx)
        .await
        .expect("delete exact-scope person identifiers");
        sqlx::query("DELETE FROM litecr.source_assertion WHERE source_record_id IN (SELECT source_record_id FROM litecr.source_record WHERE registry_id=$1 AND organization_id=$2)").bind(&registry).bind(org).execute(&mut *tx).await.expect("delete exact-scope source assertions");
        sqlx::query("DELETE FROM litecr.source_record_version WHERE source_record_id IN (SELECT source_record_id FROM litecr.source_record WHERE registry_id=$1 AND organization_id=$2)").bind(&registry).bind(org).execute(&mut *tx).await.expect("delete exact-scope source versions");
        sqlx::query("DELETE FROM litecr.source_record WHERE registry_id=$1 AND organization_id=$2")
            .bind(&registry)
            .bind(org)
            .execute(&mut *tx)
            .await
            .expect("delete exact-scope source records");
        sqlx::query("DELETE FROM litecr.person_alias WHERE registry_id=$1 AND organization_id=$2")
            .bind(&registry)
            .bind(org)
            .execute(&mut *tx)
            .await
            .expect("delete exact-scope aliases");
        sqlx::query("DELETE FROM litecr.person WHERE registry_id=$1 AND organization_id=$2")
            .bind(&registry)
            .bind(org)
            .execute(&mut *tx)
            .await
            .expect("delete exact-scope people");
        sqlx::query(
            "DELETE FROM litecr.mutation_audit WHERE registry_id=$1 AND organization_id=$2",
        )
        .bind(&registry)
        .bind(org)
        .execute(&mut *tx)
        .await
        .expect("delete exact-scope audit rows");
    }
    tx.commit().await.expect("matching cleanup commits");
    pool.close().await;
}
fn ensure(ok: bool, msg: &str) -> Result<(), String> {
    if ok { Ok(()) } else { Err(msg.into()) }
}
fn debug(err: impl std::fmt::Debug) -> String {
    format!("synthetic matching test operation failed: {err:?}")
}

#[tokio::test]
async fn postgres_indexed_exact_names_preserve_character_boundaries_after_reindex() {
    let url = std::env::var("LITECR_TEST_DATABASE_URL").expect("synthetic-only test DB required");
    let key = suffix();
    let run = tokio::spawn(exact_name_boundary_scenario(url.clone(), key.clone())).await;
    cleanup(&url, &key).await;
    run.expect("exact-name boundary scenario joined")
        .expect("short and long exact names survive the redundant-pass guard");
}

async fn exact_name_boundary_scenario(url: String, key: String) -> Result<(), String> {
    let pool = connect(&url, 2).await.map_err(debug)?;
    migrate(&pool).await.map_err(debug)?;
    let registry = format!("match-reg-{key}");
    let org = format!("match-org-{key}");
    let actor = RegistryActor {
        kind: ActorKind::System,
        id: format!("synthetic-match-seeder-{key}"),
        issuer_url: None,
        request_id: format!("exact-boundary-seed-{key}"),
        correlation_id: None,
    };
    let ingest = PostgresRegistryStore::new(pool.clone());
    let matcher = PostgresMatchingStore::new(pool.clone());
    // Byte and character bounds differ for native scripts. The ASCII one-byte
    // control must remain excluded; identical oversized names remain exact hits.
    let cases = [
        ("ascii-one", "a".to_owned(), false),
        ("ascii-two", "ab".to_owned(), true),
        ("ascii-three", "abc".to_owned(), true),
        ("lao-one", "ກ".to_owned(), true),
        ("four", "wxyz".to_owned(), true),
        ("maximum", "x".repeat(128), true),
        ("above-maximum", "y".repeat(129), true),
    ];
    let mut pairs = Vec::new();
    for (label, name, expected_hit) in cases {
        let mut ids = Vec::new();
        for side in ["query", "candidate"] {
            let record = format!("exact-boundary-{label}-{side}");
            let mut cmd = command(&registry, &org, &key, &record, &name, "");
            cmd.assertions.truncate(1);
            cmd.assertions[0].language_tag = None;
            cmd.assertions[0].script = None;
            let receipt = ingest
                .ingest(&actor, &format!("exact-{record}-{key}"), &cmd)
                .await
                .map_err(debug)?;
            ids.push(receipt.source_record_id);
        }
        pairs.push((label, ids.remove(0), ids.remove(0), expected_hit));
    }
    // Force the indexed path after a real bounded reindex. No DOB/identifier
    // blocks or legacy fallback may hide a lost exact-name boundary candidate.
    sqlx::query(
        "DELETE FROM matching.indexed_source_version WHERE source_record_id IN \
         (SELECT source_record_id FROM litecr.source_record \
          WHERE registry_id=$1 AND organization_id=$2)",
    )
    .bind(&registry)
    .bind(&org)
    .execute(&pool)
    .await
    .map_err(debug)?;
    let reindexed = matcher
        .reindex_next_batch(Some(&registry), Some(&org), 100)
        .await
        .map_err(debug)?;
    ensure(reindexed == 14, "reindex must cover every boundary fixture")?;
    let current_markers: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM matching.indexed_source_version i \
         JOIN litecr.source_record sr ON sr.source_record_id=i.source_record_id \
         WHERE sr.registry_id=$1 AND sr.organization_id=$2 \
           AND i.receive_version=sr.current_receive_version AND i.normalizer_version=$3",
    )
    .bind(&registry)
    .bind(&org)
    .bind(litecr_matching::NORMALIZATION_VERSION)
    .fetch_one(&pool)
    .await
    .map_err(debug)?;
    ensure(
        current_markers == 14,
        "all boundary sources must have current markers so legacy fallback cannot hide loss",
    )?;
    for (label, query_id, candidate_id, expected_hit) in pairs {
        let result = matcher
            .evaluate(
                &registry,
                &org,
                &query_id,
                20,
                "synthetic-matcher",
                &format!("exact-boundary-{label}-{key}"),
                None,
            )
            .await
            .map_err(debug)?;
        let actual_ids: Vec<_> = result
            .candidates
            .iter()
            .map(|candidate| candidate.candidate_record_id.as_str())
            .collect();
        let expected_ids = if expected_hit {
            vec![candidate_id.as_str()]
        } else {
            vec![]
        };
        ensure(
            actual_ids == expected_ids,
            &format!("{label} must preserve exactly the expected indexed candidate"),
        )?;
        ensure(
            !result.retrieval.truncated,
            "small boundary fixtures must never truncate",
        )?;
    }
    pool.close().await;
    Ok(())
}

#[tokio::test]
async fn candidate_jit_setting_does_not_escape_pooled_connection() {
    let url = std::env::var("LITECR_TEST_DATABASE_URL").expect("synthetic-only test DB required");
    let pool = connect(&url, 1).await.expect("test DB reachable");
    migrate(&pool).await.expect("schema migrated");
    sqlx::query("SET jit=on").execute(&pool).await.unwrap();
    let matcher = PostgresMatchingStore::new(pool.clone());
    let result = matcher
        .evaluate(
            "jit-synthetic",
            "jit-synthetic",
            "00000000-0000-0000-0000-000000000000",
            20,
            "synthetic-actor",
            "synthetic-request",
            None,
        )
        .await;
    assert!(matches!(result, Err(MatchingStoreError::NotFound)));
    let setting: String = sqlx::query_scalar("SELECT current_setting('jit')")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        setting, "on",
        "candidate-specific setting leaked to pooled connection"
    );
    let failed = matcher
        .evaluate(
            "jit-synthetic",
            "jit-synthetic",
            "invalid-uuid",
            20,
            "synthetic-actor",
            "synthetic-request-failed",
            None,
        )
        .await;
    assert!(matches!(failed, Err(MatchingStoreError::Unavailable)));
    let restored: String = sqlx::query_scalar("SELECT current_setting('jit')")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        restored, "on",
        "failed candidate statement leaked JIT policy"
    );
    pool.close().await;
}
