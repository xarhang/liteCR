use litecr_infrastructure::{connect, migrate};
use serde_json::Value;

const BASELINE: &str = include_str!("../migrations/0027_bounded_name_retrieval.sql");
const GUARD_MIGRATION: &str = include_str!("../migrations/0031_name_edit_guard.sql");

#[tokio::test]
async fn guarded_edit_and_dependent_helpers_preserve_baseline_results_and_function_contract() {
    let database_url = std::env::var("LITECR_TEST_DATABASE_URL")
        .expect("set LITECR_TEST_DATABASE_URL to the dedicated synthetic-only litecr_test DB");
    let pool = connect(&database_url, 1)
        .await
        .expect("synthetic owner connects");
    let database: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(&pool)
        .await
        .expect("read synthetic database name");
    assert_eq!(
        database, "litecr_test",
        "requires the dedicated synthetic DB"
    );
    migrate(&pool).await.expect("current migrations apply");
    let mut tx = pool.begin().await.expect("helper fixture transaction");
    // Temporary functions retain the original SQL implementation independently
    // of the new helper. A rollback removes all diagnostic DDL and metadata.
    sqlx::raw_sql("CREATE TEMP TABLE name_guard_owner_scope(singleton boolean)")
        .execute(&mut *tx)
        .await
        .expect("initialize this session's temporary namespace");
    let start = BASELINE
        .find("CREATE FUNCTION matching.name_edit_within_v1")
        .unwrap();
    let end = BASELINE.find("\nDO $$").unwrap();
    let original_helpers = &BASELINE[start..end];
    let temporary_helpers = original_helpers.replace("matching.", "pg_temp.");
    sqlx::raw_sql(&temporary_helpers)
        .execute(&mut *tx)
        .await
        .expect("install transaction-local original helpers");

    // Exercise the actual CREATE OR REPLACE migration and verify that function
    // identity/privileges and execution attributes survive the body replacement.
    // Restoring the old body and schema marker is transaction-local only.
    let restore = original_helpers.replace("CREATE FUNCTION", "CREATE OR REPLACE FUNCTION");
    sqlx::raw_sql(&restore)
        .execute(&mut *tx)
        .await
        .expect("temporarily restore the baseline helper bodies");
    let before: Value = sqlx::query_scalar(FUNCTION_CONTRACT)
        .fetch_one(&mut *tx)
        .await
        .expect("baseline function contract");
    sqlx::query("UPDATE litecr.schema_meta SET version=30 WHERE singleton=TRUE")
        .execute(&mut *tx)
        .await
        .expect("temporary pre-migration schema marker");
    sqlx::raw_sql(GUARD_MIGRATION)
        .execute(&mut *tx)
        .await
        .expect("actual guard migration applies");
    let after: Value = sqlx::query_scalar(FUNCTION_CONTRACT)
        .fetch_one(&mut *tx)
        .await
        .expect("updated function contract");
    assert_eq!(
        after, before,
        "OID, ACL and execution attributes are preserved"
    );

    let mut strings = vec![String::new()];
    let mut level = vec![String::new()];
    for _ in 0..4 {
        level = level
            .iter()
            .flat_map(|prefix| ['a', 'b', 'c'].map(|c| format!("{prefix}{c}")))
            .collect();
        strings.extend(level.clone());
    }
    assert_eq!(strings.len(), 121);
    let exhaustive: i64 = sqlx::query_scalar(EDIT_DIFFERENCES)
        .bind(&strings)
        .bind(vec![0_i32, 1, 2])
        .fetch_one(&mut *tx)
        .await
        .expect("exhaustive short-string OSA comparison");
    assert_eq!(exhaustive, 0, "43,923 bounded edit comparisons agree");

    let controls: Vec<Option<String>> = vec![
        None,
        Some("".into()),
        Some("a".into()),
        Some("ab".into()),
        Some("ba".into()),
        Some("abc".into()),
        Some("acb".into()),
        Some("aaaa".into()),
        Some("aaab".into()),
        Some("aaaab".into()),
        Some("somchai".into()),
        Some("somchay".into()),
        Some("som chai".into()),
        Some("chai som".into()),
        Some("som som chai".into()),
        Some("som chai chai".into()),
        Some("ສົມໃຈ".into()),
        Some("ສົມໄຈ".into()),
        Some("ສົມ ໃຈ".into()),
        Some("สมชาย".into()),
        Some("สมชย".into()),
        Some("é".into()),
        Some("e\u{301}".into()),
        Some("李 明".into()),
        Some("明 李".into()),
        Some("a ".repeat(8)),
        Some("a ".repeat(9)),
        Some("a".repeat(127)),
        Some("a".repeat(128)),
        Some("a".repeat(129)),
        Some("ກ".repeat(128)),
        Some("ກ".repeat(129)),
        Some(format!("{}b", "a".repeat(127))),
        Some(format!("{}bc", "a".repeat(126))),
        Some(format!("{}cb", "a".repeat(126))),
    ];
    let boundary_differences: i64 = sqlx::query_scalar(EDIT_DIFFERENCES)
        .bind(&controls)
        .bind(vec![Some(-1_i32), Some(0), Some(1), Some(2), Some(3), None])
        .fetch_one(&mut *tx)
        .await
        .expect("Unicode, repeated-character, transposition and invalid-bound comparisons");
    assert_eq!(boundary_differences, 0);
    let helper_differences: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM unnest($1::text[]) a(value) CROSS JOIN unnest($1::text[]) b(value)
         WHERE matching.name_prefix_within_v1(a.value,b.value)
                   IS DISTINCT FROM pg_temp.name_prefix_within_v1(a.value,b.value)
            OR matching.name_parts_within_v1(a.value,b.value)
                   IS DISTINCT FROM pg_temp.name_parts_within_v1(a.value,b.value)",
    )
    .bind(&controls)
    .fetch_one(&mut *tx)
    .await
    .expect("prefix and distinct-token assignment dependent-helper parity");
    assert_eq!(helper_differences, 0);
    tx.rollback()
        .await
        .expect("remove diagnostic helper DDL and restore metadata");
    pool.close().await;
}

const EDIT_DIFFERENCES: &str = "SELECT count(*) FROM unnest($1::text[]) a(value)
     CROSS JOIN unnest($1::text[]) b(value) CROSS JOIN unnest($2::integer[]) k(value)
     WHERE matching.name_edit_within_v1(a.value,b.value,k.value)
       IS DISTINCT FROM pg_temp.name_edit_within_v1(a.value,b.value,k.value)";

const FUNCTION_CONTRACT: &str = "SELECT jsonb_build_object('oid',oid,'owner',proowner,'acl',proacl,
       'volatility',provolatile,'parallel',proparallel,'strict',proisstrict,
       'security_definer',prosecdef,'configuration',proconfig)
     FROM pg_proc WHERE oid='matching.name_edit_within_v1(text,text,integer)'::regprocedure";
