use litecr_infrastructure::{connect, migrate};
use serde_json::Value;

const BEFORE: &str = include_str!("fixtures/registry-alias-before.sql");
const REGISTRY_SOURCE: &str = include_str!("../src/registry/mod.rs");

#[tokio::test]
async fn alias_lookup_preserves_results_and_bounds_work_with_wrong_scope_statistics() {
    let url = std::env::var("LITECR_TEST_DATABASE_URL")
        .expect("set LITECR_TEST_DATABASE_URL to the dedicated synthetic-only litecr_test DB");
    let pool = connect(&url, 1).await.expect("synthetic owner connects");
    let database: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(&pool)
        .await
        .expect("synthetic database name");
    assert_eq!(database, "litecr_test", "requires dedicated synthetic DB");
    migrate(&pool).await.expect("current migrations apply");
    let mut tx = pool.begin().await.expect("isolated alias-plan transaction");
    let registry: String = sqlx::query_scalar("SELECT 'alias-plan-' || gen_random_uuid()::text")
        .fetch_one(&mut *tx)
        .await
        .expect("unique synthetic scope");
    sqlx::raw_sql(
        "CREATE TEMP TABLE alias_plan_fixture AS
           SELECT i,gen_random_uuid() pid,gen_random_uuid() sid,
                  gen_random_uuid() cid,gen_random_uuid() did,gen_random_uuid() mid
           FROM generate_series(1,10000) i;",
    )
    .execute(&mut *tx)
    .await
    .expect("synthetic UUID fixture");
    sqlx::query(
        "INSERT INTO litecr.person(person_id,public_id,registry_id,organization_id)
         SELECT pid,$1 || '-' || i,$1,'org' FROM alias_plan_fixture",
    )
    .bind(&registry)
    .execute(&mut *tx)
    .await
    .expect("10k synthetic persons");
    sqlx::query(
        "INSERT INTO litecr.person_alias(registry_id,organization_id,alias_value,person_id)
         SELECT $1,'org','alias-' || lpad(i::text,5,'0'),pid FROM alias_plan_fixture",
    )
    .bind(&registry)
    .execute(&mut *tx)
    .await
    .expect("10k aliases in one scope");
    sqlx::query(
        "INSERT INTO litecr.source_record(source_record_id,registry_id,organization_id,source_system_id,source_record_key,person_id)
         SELECT sid,$1,'org','synthetic',i::text,pid FROM alias_plan_fixture WHERE i<=3",
    ).bind(&registry).execute(&mut *tx).await.expect("graph source records");
    sqlx::raw_sql(
        "INSERT INTO litecr.source_record_version(source_record_id,receive_version,payload_hash,version_state)
         SELECT sid,1,decode(repeat('00',32),'hex'),'accepted' FROM alias_plan_fixture WHERE i<=3;",
    ).execute(&mut *tx).await.expect("graph source versions");
    sqlx::query(
        "INSERT INTO litecr.review_case(case_id,registry_id,organization_id,query_source_record_id,candidate_source_record_id,query_receive_version,candidate_receive_version,created_by)
         SELECT f.cid,$1,'org',f.sid,t.sid,1,1,'synthetic'
         FROM alias_plan_fixture f JOIN alias_plan_fixture t ON t.i=f.i+1 WHERE f.i<=2",
    ).bind(&registry).execute(&mut *tx).await.expect("graph review fixtures");
    sqlx::raw_sql(
        "INSERT INTO litecr.review_decision(decision_id,case_id,case_version,decision,actor_id,reason_code,reason,idempotency_key_hash,command_fingerprint,outcome)
         SELECT did,cid,1,'merge','synthetic','synthetic','Synthetic plan fixture only',decode(repeat('00',32),'hex'),decode(repeat('00',32),'hex'),'accepted'
         FROM alias_plan_fixture WHERE i<=2;",
    ).execute(&mut *tx).await.expect("graph decision fixtures");
    sqlx::query(
        "INSERT INTO litecr.identity_merge(merge_id,registry_id,organization_id,source_person_id,target_person_id,decision_id,source_version_before,target_version_before,source_version_after,target_version_after)
         SELECT f.mid,$1,'org',f.pid,t.pid,f.did,1,1,2,2
         FROM alias_plan_fixture f JOIN alias_plan_fixture t ON t.i=f.i+1 WHERE f.i<=2",
    ).bind(&registry).execute(&mut *tx).await.expect("historical merge graph fixtures");
    sqlx::query(
        "INSERT INTO litecr.person_redirect(registry_id,organization_id,source_person_id,target_person_id,merge_id)
         SELECT $1,'org',f.pid,t.pid,f.mid
         FROM alias_plan_fixture f JOIN alias_plan_fixture t ON t.i=f.i+1 WHERE f.i<=2",
    ).bind(&registry).execute(&mut *tx).await.expect("two-hop incoming redirect graph");

    // Controlled owner-only planner fault injection, confined to this transaction.
    // PostgreSQL rolls these catalog changes and all fixtures back at the end.
    // This is not a production maintenance recommendation or an HTTP cache test.
    sqlx::raw_sql(
        "DELETE FROM pg_catalog.pg_statistic WHERE starelid='litecr.person_alias'::regclass;
         UPDATE pg_catalog.pg_class SET reltuples=1,relpages=1,relallvisible=0
           WHERE oid='litecr.person_alias'::regclass;
         SET LOCAL jit=off;
         SET LOCAL plan_cache_mode=force_generic_plan;",
    )
    .execute(&mut *tx)
    .await
    .expect("transaction-local wrong scope estimates");

    let target: String = sqlx::query_scalar("SELECT pid::text FROM alias_plan_fixture WHERE i=3")
        .fetch_one(&mut *tx)
        .await
        .expect("graph target UUID");
    let old = BEFORE.trim().trim_end_matches(';');
    let new_sql = runtime_alias_query();
    let new = new_sql.as_str();
    for (organization, expected) in [
        ("org", vec!["alias-00001", "alias-00002", "alias-00003"]),
        ("foreign-org", Vec::new()),
    ] {
        let baseline: Vec<String> = sqlx::query_scalar(old)
            .bind(&registry)
            .bind(organization)
            .bind(&target)
            .fetch_all(&mut *tx)
            .await
            .expect("baseline alias results");
        let actual: Vec<String> = sqlx::query_scalar(new)
            .bind(&registry)
            .bind(organization)
            .bind(&target)
            .fetch_all(&mut *tx)
            .await
            .expect("bounded alias results");
        assert_eq!(actual, baseline, "exact old/new alias projection parity");
        assert_eq!(
            actual, expected,
            "sorted incoming aliases and scope exclusion"
        );
    }
    let plan: Value = sqlx::query_scalar(&format!("EXPLAIN (ANALYZE,BUFFERS,FORMAT JSON) {new}"))
        .bind(&registry)
        .bind("org")
        .bind(&target)
        .fetch_one(&mut *tx)
        .await
        .expect("actual generic alias plan");
    let mut alias_visits = 0.0;
    let mut alias_scans = 0;
    inspect_alias_scans(&plan, &mut alias_visits, &mut alias_scans);
    assert!(alias_scans > 0, "plan includes actual alias access");
    assert!(
        alias_visits <= 6.0,
        "three-person graph must not scan 10k aliases under wrong estimates: visits={alias_visits}"
    );
    tx.rollback()
        .await
        .expect("restore catalogs and remove all graph fixtures");
    pool.close().await;
}

fn runtime_alias_query() -> String {
    let marker = "\"WITH RECURSIVE alias_people";
    assert_eq!(
        REGISTRY_SOURCE.matches(marker).count(),
        1,
        "runtime alias SQL must have one unique quoted marker"
    );
    let start = REGISTRY_SOURCE
        .find(marker)
        .expect("runtime alias SQL marker")
        + 1;
    let tail = &REGISTRY_SOURCE[start..];
    let end = tail.find('"').expect("runtime alias SQL closing quote");
    // The SQL contains no embedded double quotes. Replace Rust continuation
    // escapes with SQL whitespace, preserving token separation for CRLF and LF.
    tail[..end].replace("\\\r\n", " ").replace("\\\n", " ")
}

fn inspect_alias_scans(value: &Value, visits: &mut f64, scans: &mut usize) {
    match value {
        Value::Object(fields) => {
            if fields.get("Relation Name").and_then(Value::as_str) == Some("person_alias") {
                assert!(
                    fields
                        .get("Index Cond")
                        .and_then(Value::as_str)
                        .is_some_and(|condition| condition.contains("person_id")),
                    "alias access must use a person-key index condition, not a scope-only scan"
                );
                let number = |key| fields.get(key).and_then(Value::as_f64).unwrap_or(0.0);
                *visits += (number("Actual Rows")
                    + number("Rows Removed by Filter")
                    + number("Rows Removed by Index Recheck"))
                    * number("Actual Loops");
                *scans += 1;
            }
            for child in fields.values() {
                inspect_alias_scans(child, visits, scans);
            }
        }
        Value::Array(items) => {
            for child in items {
                inspect_alias_scans(child, visits, scans);
            }
        }
        _ => {}
    }
}
