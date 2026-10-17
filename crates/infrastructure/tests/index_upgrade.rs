use std::{borrow::Cow, str::FromStr, time::SystemTime};

use litecr_infrastructure::{MIGRATOR, connect};
use sqlx::{PgPool, migrate::Migrator, postgres::PgConnectOptions, postgres::PgPoolOptions};

#[tokio::test]
async fn populated_schema28_index_upgrade_preserves_identity_and_constraints() {
    let Ok(database_url) = std::env::var("LITECR_TEST_DATABASE_URL") else {
        eprintln!("skipping index upgrade test: LITECR_TEST_DATABASE_URL is unset");
        return;
    };
    let owner = connect(&database_url, 1)
        .await
        .expect("synthetic owner connects");
    let suffix = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .expect("clock is after Unix epoch")
        .as_nanos();
    // Only our fixed prefix and numeric process/time suffix enter SQL identifiers.
    let database = format!("litecr_index_upgrade_{}_{suffix}", std::process::id());
    let options = PgConnectOptions::from_str(&database_url)
        .expect("test owner URL is valid")
        .database(&database);
    sqlx::query(&format!("CREATE DATABASE {database}"))
        .execute(&owner)
        .await
        .expect("create owned disposable database");
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect_lazy_with(options);
    let scenario_pool = pool.clone();
    // A failed assertion is captured, allowing cleanup before propagating it.
    let outcome = tokio::spawn(async move { upgrade_scenario(&scenario_pool).await }).await;
    pool.close().await;
    let cleanup: Result<(), sqlx::Error> = async {
        let owned: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM pg_database WHERE datname=$1
             AND pg_get_userbyid(datdba)=current_user)",
        )
        .bind(&database)
        .fetch_one(&owner)
        .await?;
        if !owned {
            return Err(sqlx::Error::Protocol(
                "refusing cleanup: generated test database is not owned by current_user".into(),
            ));
        }
        sqlx::query(&format!("DROP DATABASE {database} WITH (FORCE)"))
            .execute(&owner)
            .await?;
        Ok(())
    }
    .await;
    owner.close().await;
    if let Err(error) = &cleanup {
        eprintln!("owned index-upgrade database cleanup failed: {error}");
    }
    match outcome {
        Err(error) if error.is_panic() => std::panic::resume_unwind(error.into_panic()),
        Err(error) => panic!("index upgrade scenario task failed: {error}"),
        Ok(Err(error)) => panic!("index upgrade scenario failed: {error}"),
        Ok(Ok(())) => {}
    }
    cleanup.expect("drop only this test's owned database after closing its pool");
}

async fn upgrade_scenario(pool: &PgPool) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let schema28 = Migrator {
        migrations: Cow::Owned(
            MIGRATOR
                .iter()
                .filter(|m| m.version <= 28)
                .cloned()
                .collect(),
        ),
        ..Migrator::DEFAULT
    };
    schema28.run(pool).await?;
    let version: i64 = sqlx::query_scalar("SELECT version FROM litecr.schema_meta WHERE singleton")
        .fetch_one(pool)
        .await?;
    assert_eq!(version, 28);
    let old_index: bool = sqlx::query_scalar(
        "SELECT to_regclass('litecr.source_assertion_scoped_path_idx') IS NOT NULL",
    )
    .fetch_one(pool)
    .await?;
    assert!(old_index);
    sqlx::raw_sql(
        "INSERT INTO litecr.person(person_id,public_id,registry_id,organization_id) VALUES
          ('00000000-0000-0000-0000-000000000001','upgrade-person-1','upgrade','org'),
          ('00000000-0000-0000-0000-000000000002','upgrade-person-2','upgrade','org');
         INSERT INTO litecr.source_record(source_record_id,registry_id,organization_id,source_system_id,source_record_key,person_id)
          SELECT person_id,'upgrade','org','synthetic',public_id,person_id FROM litecr.person;
         INSERT INTO litecr.source_record_version(source_record_id,receive_version,source_version,payload_hash,version_state)
          SELECT source_record_id,1,'v1',decode(repeat('01',32),'hex'),'accepted' FROM litecr.source_record;
         INSERT INTO litecr.source_assertion(source_record_id,receive_version,field_path,original_value,language_tag,script)
          SELECT source_record_id,1,'name.original','ສົມໃຈ ' || source_record_key,'lo','Lao' FROM litecr.source_record;
         INSERT INTO litecr.source_identifier_assertion(source_record_id,receive_version,registry_id,organization_id,authority,identifier_type,original_value,authoritative,assertion_state)
          SELECT source_record_id,1,'upgrade','org','synthetic-household','household','shared-value',FALSE,'accepted' FROM litecr.source_record;
         INSERT INTO litecr.person_alias(registry_id,organization_id,alias_value,person_id)
          SELECT 'upgrade','org','alias-' || public_id,person_id FROM litecr.person;",
    ).execute(pool).await?;
    let before = identity_snapshot(pool).await?;
    let schema29 = Migrator {
        migrations: Cow::Owned(
            MIGRATOR
                .iter()
                .filter(|m| m.version <= 29)
                .cloned()
                .collect(),
        ),
        ..Migrator::DEFAULT
    };
    schema29.run(pool).await?;
    schema29.run(pool).await?;
    assert_eq!(
        identity_snapshot(pool).await?,
        before,
        "index-only upgrade preserves complete rows"
    );
    let shared: (i64, i64) = sqlx::query_as(
        "SELECT count(*),count(DISTINCT sr.person_id) FROM litecr.source_identifier_assertion a
         JOIN litecr.source_record sr USING(source_record_id)
         WHERE a.original_value='shared-value' AND NOT a.authoritative",
    )
    .fetch_one(pool)
    .await?;
    assert_eq!(
        shared,
        (2, 2),
        "shared household identifiers remain distinct people"
    );
    let canonical: i64 = sqlx::query_scalar("SELECT count(*) FROM litecr.person_identifier")
        .fetch_one(pool)
        .await?;
    assert_eq!(
        canonical, 0,
        "indexes never promote nonauthoritative assertions"
    );
    let valid: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pg_index i JOIN pg_class c ON c.oid=i.indexrelid
         JOIN pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname='litecr'
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
    .fetch_one(pool)
    .await?;
    assert_eq!(valid, 18);
    let old_index: bool = sqlx::query_scalar(
        "SELECT to_regclass('litecr.source_assertion_scoped_path_idx') IS NOT NULL",
    )
    .fetch_one(pool)
    .await?;
    assert!(!old_index, "only the redundant index is removed");
    let duplicate = sqlx::query(
        "INSERT INTO litecr.source_assertion SELECT * FROM litecr.source_assertion LIMIT 1",
    )
    .execute(pool)
    .await
    .expect_err("source assertion primary key still rejects duplicates");
    assert_eq!(
        duplicate
            .as_database_error()
            .and_then(|error| error.code())
            .as_deref(),
        Some("23505")
    );
    let state: (i64, i64) = sqlx::query_as(
        "SELECT version,(SELECT count(*) FROM public._sqlx_migrations WHERE version=29 AND success)
         FROM litecr.schema_meta WHERE singleton",
    )
    .fetch_one(pool)
    .await?;
    assert_eq!(
        state,
        (29, 1),
        "retry retains one successful migration ledger entry"
    );
    assert_eq!(identity_snapshot(pool).await?, before);
    Ok(())
}

async fn identity_snapshot(pool: &PgPool) -> Result<Vec<String>, sqlx::Error> {
    let mut snapshot = Vec::new();
    for table in [
        "person",
        "source_record",
        "source_record_version",
        "source_assertion",
        "source_identifier_assertion",
        "person_alias",
    ] {
        let rows: String = sqlx::query_scalar(&format!(
            "SELECT jsonb_agg(doc ORDER BY doc::text)::text FROM (SELECT to_jsonb(t) AS doc FROM litecr.{table} t) rows",
        )).fetch_one(pool).await?;
        snapshot.push(rows);
    }
    Ok(snapshot)
}
