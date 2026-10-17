use litecr_application::ReadinessProbe;
use litecr_domain::{SCHEMA_VERSION, SchemaVersion};
use litecr_infrastructure::{
    MIGRATOR, PostgresReadinessProbe, connect, migrate, schema_version,
    synchronous_replication_status,
};
use sqlx::Row;

#[tokio::test]
async fn postgres_migration_retry_rollback_readiness_and_schema_guard() {
    let Ok(database_url) = std::env::var("LITECR_TEST_DATABASE_URL") else {
        eprintln!(
            "skipping PostgreSQL foundation integration test: LITECR_TEST_DATABASE_URL is unset"
        );
        return;
    };

    let pool = connect(&database_url, 2)
        .await
        .expect("isolated test PostgreSQL reachable");
    migrate(&pool)
        .await
        .expect("first embedded migration succeeds");
    migrate(&pool).await.expect("migration retry is idempotent");

    let identifier_index: (bool, bool, bool) = sqlx::query_as(
        "SELECT i.indisvalid,i.indisready,NOT i.indisunique \
         FROM pg_index i JOIN pg_class c ON c.oid=i.indexrelid \
         JOIN pg_namespace n ON n.oid=c.relnamespace \
         WHERE n.nspname='litecr' AND c.relname='source_identifier_assertion_live_source_version_idx'",
    )
    .fetch_one(&pool)
    .await
    .expect("source/version identifier index exists after migration and retry");
    assert_eq!(identifier_index, (true, true, true));

    let query_indexes: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pg_index i JOIN pg_class c ON c.oid=i.indexrelid \
         JOIN pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname='litecr' \
           AND c.relname=ANY($1::text[]) AND i.indisvalid AND i.indisready AND NOT i.indisunique",
    )
    .bind(
        &[
            "person_alias_person_scope_idx",
            "person_redirect_target_lookup_idx",
            "person_identifier_person_scope_idx",
            "source_identifier_accepted_value_idx",
            "source_record_person_lookup_idx",
            "review_case_scope_cursor_idx",
            "migration_journal_batch_lookup_idx",
            "migration_journal_imported_source_idx",
            "review_case_query_source_lookup_idx",
            "review_case_candidate_source_lookup_idx",
            "identity_merge_member_source_lookup_idx",
            "confirmed_non_match_left_lookup_idx",
            "confirmed_non_match_right_lookup_idx",
            "identity_link_source_lookup_idx",
            "identity_link_target_lookup_idx",
            "identity_merge_source_lookup_idx",
            "identity_merge_target_lookup_idx",
            "person_redirect_source_lookup_idx",
        ][..],
    )
    .fetch_one(&pool)
    .await
    .expect("query-path indexes survive fresh migration and retry");
    assert_eq!(query_indexes, 18);
    let redundant: Option<String> =
        sqlx::query_scalar("SELECT to_regclass('litecr.source_assertion_scoped_path_idx')::text")
            .fetch_one(&pool)
            .await
            .expect("redundant index lookup succeeds");
    assert!(redundant.is_none());

    let extension = sqlx::query("SELECT 1 FROM pg_extension WHERE extname = 'pg_trgm'")
        .fetch_optional(&pool)
        .await
        .expect("extension catalog query succeeds");
    assert!(extension.is_some(), "pg_trgm extension is installed");

    let version = schema_version(&pool)
        .await
        .expect("schema metadata query succeeds");
    assert_eq!(version, Some(SchemaVersion::new(SCHEMA_VERSION)));
    let standby_setting: String =
        sqlx::query_scalar("SELECT current_setting('synchronous_standby_names')")
            .fetch_one(&pool)
            .await
            .expect("standby policy setting is readable without disclosing its value");
    let replication_status = synchronous_replication_status(&pool)
        .await
        .expect("name-free live replication status can be read");
    assert_eq!(
        replication_status.policy_configured,
        !standby_setting.trim().is_empty()
    );
    assert!(replication_status.streaming_standbys >= 0);
    assert!(replication_status.synchronous_standbys <= replication_status.streaming_standbys);
    assert!(replication_status.quorum_candidates <= replication_status.streaming_standbys);
    assert!(replication_status.asynchronous_standbys <= replication_status.streaming_standbys);
    assert!(replication_status.potential_standbys <= replication_status.streaming_standbys);
    let migration_versions = MIGRATOR
        .iter()
        .map(|migration| migration.version)
        .collect::<Vec<_>>();
    assert!(migration_versions.contains(&1));
    assert!(migration_versions.contains(&2));
    assert!(migration_versions.contains(&3));
    assert!(migration_versions.contains(&4));
    assert!(migration_versions.contains(&5));
    assert!(migration_versions.contains(&6));
    assert!(migration_versions.contains(&7));
    assert!(migration_versions.contains(&8));
    assert!(migration_versions.contains(&9));
    assert!(migration_versions.contains(&10));
    assert!(migration_versions.contains(&11));
    assert!(migration_versions.contains(&12));
    assert!(migration_versions.contains(&13));
    assert!(migration_versions.contains(&14));
    assert!(migration_versions.contains(&15));
    assert!(migration_versions.contains(&17));
    assert!(migration_versions.contains(&18));
    assert!(migration_versions.contains(&19));
    assert!(migration_versions.contains(&20));
    assert!(migration_versions.contains(&21));
    assert!(migration_versions.contains(&22));
    assert!(migration_versions.contains(&23));
    assert!(migration_versions.contains(&24));
    assert!(migration_versions.contains(&25));
    assert!(migration_versions.windows(2).all(|pair| pair[0] < pair[1]));

    let value_search_index = sqlx::query_scalar::<_, bool>(
        "SELECT to_regclass('litecr.person_identifier_scoped_token_lookup_idx') IS NOT NULL",
    )
    .fetch_one(&pool)
    .await
    .expect("scoped identifier lookup index query succeeds");
    assert!(
        value_search_index,
        "FHIR token searches need a scoped value index"
    );

    let key = format!(
        "rollback-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_micros()
    );
    let mut transaction = pool.begin().await.expect("transaction begins");
    sqlx::query(
        "INSERT INTO litecr.foundation_probe (probe_key, probe_value) VALUES ($1, 'synthetic')",
    )
    .bind(&key)
    .execute(&mut *transaction)
    .await
    .expect("synthetic probe insert succeeds");
    transaction
        .rollback()
        .await
        .expect("transaction rolls back");
    let row =
        sqlx::query("SELECT COUNT(*) AS count FROM litecr.foundation_probe WHERE probe_key = $1")
            .bind(key)
            .fetch_one(&pool)
            .await
            .expect("rollback verification query succeeds");
    assert_eq!(row.get::<i64, _>("count"), 0);

    let readiness = PostgresReadinessProbe::new(pool.clone())
        .snapshot()
        .await
        .expect("readiness probe succeeds");
    assert!(readiness.is_ready());
    pool.close().await;
}
