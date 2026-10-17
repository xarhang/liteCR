use std::{borrow::Cow, collections::BTreeSet, str::FromStr, time::SystemTime};

use litecr_domain::SCHEMA_VERSION;
use litecr_infrastructure::{MIGRATOR, connect, migrate};
use serde_json::Value;
use sqlx::{PgPool, migrate::Migrator, postgres::PgConnectOptions, postgres::PgPoolOptions};

type TestError = Box<dyn std::error::Error + Send + Sync>;

#[tokio::test]
async fn populated_marker_partition_upgrade_rollback_acl_and_pruning() {
    let Ok(url) = std::env::var("LITECR_TEST_DATABASE_URL") else {
        eprintln!("skipping marker partition test: LITECR_TEST_DATABASE_URL is unset");
        return;
    };
    let owner = connect(&url, 1).await.expect("synthetic owner connects");
    let suffix = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .expect("clock is after Unix epoch")
        .as_nanos();
    let database = format!("litecr_partition_{}_{suffix}", std::process::id());
    let runtime = format!("lp_runtime_{}_{suffix}", std::process::id());
    let worker = format!("lp_worker_{}_{suffix}", std::process::id());
    // These SQL identifiers contain only fixed ASCII prefixes and numeric suffixes.
    let options = PgConnectOptions::from_str(&url)
        .expect("valid synthetic URL")
        .database(&database)
        // Match the application's fixed migration-ledger namespace.
        .options([("search_path", "public")]);
    sqlx::query(&format!("CREATE DATABASE {database}"))
        .execute(&owner)
        .await
        .expect("create this test's disposable database");
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect_lazy_with(options);
    let scenario_pool = pool.clone();
    let scenario_runtime = runtime.clone();
    let scenario_worker = worker.clone();
    let local = tokio::task::LocalSet::new();
    let outcome = local
        .run_until(async move {
            tokio::task::spawn_local(async move {
                partition_scenario(&scenario_pool, &scenario_runtime, &scenario_worker).await
            })
            .await
        })
        .await;
    pool.close().await;
    let cleanup: Result<(), sqlx::Error> = async {
        let owned: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM pg_database WHERE datname=$1 AND pg_get_userbyid(datdba)=current_user)",
        ).bind(&database).fetch_one(&owner).await?;
        if !owned {
            return Err(sqlx::Error::Protocol("refusing cleanup of database not owned by current_user".into()));
        }
        sqlx::query(&format!("DROP DATABASE {database} WITH (FORCE)")).execute(&owner).await?;
        // Dropping the owned database first removes its default-ACL dependencies.
        sqlx::query(&format!("DROP ROLE IF EXISTS {runtime}")).execute(&owner).await?;
        sqlx::query(&format!("DROP ROLE IF EXISTS {worker}")).execute(&owner).await?;
        Ok(())
    }.await;
    owner.close().await;
    if let Err(error) = &cleanup {
        eprintln!("owned partition test cleanup failed: {error}");
    }
    match outcome {
        Err(error) if error.is_panic() => std::panic::resume_unwind(error.into_panic()),
        Err(error) => panic!("partition scenario task failed: {error}"),
        Ok(Err(error)) => panic!("partition scenario failed: {error}"),
        Ok(Ok(())) => {}
    }
    cleanup.expect("owned partition database and generated roles are removed");
}

async fn partition_scenario(pool: &PgPool, runtime: &str, worker: &str) -> Result<(), TestError> {
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
    sqlx::query(&format!("CREATE ROLE {runtime} NOLOGIN"))
        .execute(pool)
        .await?;
    sqlx::query(&format!("CREATE ROLE {worker} NOLOGIN"))
        .execute(pool)
        .await?;
    sqlx::raw_sql(&format!(
        "GRANT USAGE ON SCHEMA matching TO {runtime},{worker};
         GRANT SELECT,INSERT ON TABLE matching.indexed_source_version TO {runtime} WITH GRANT OPTION;
         GRANT SELECT ON TABLE matching.indexed_source_version TO {worker};
         ALTER DEFAULT PRIVILEGES IN SCHEMA matching GRANT SELECT,INSERT ON TABLES TO {worker};"
    )).execute(pool).await?;
    sqlx::raw_sql(
        "INSERT INTO litecr.person(person_id,public_id,registry_id,organization_id)
          SELECT ('00000000-0000-0000-0000-' || lpad(i::text,12,'0'))::uuid,
                 'partition-person-' || i,'partition','org' FROM generate_series(1,128) i;
         INSERT INTO litecr.source_record(source_record_id,registry_id,organization_id,source_system_id,source_record_key,person_id)
          SELECT person_id,'partition','org','synthetic',public_id,person_id FROM litecr.person;
         INSERT INTO litecr.source_record_version(source_record_id,receive_version,source_version,payload_hash,version_state)
          SELECT source_record_id,1,'v1',decode(repeat('01',32),'hex'),'accepted' FROM litecr.source_record;
         INSERT INTO matching.indexed_source_version(source_record_id,receive_version,normalizer_version,indexed_at)
          SELECT source_record_id,1,v,'2026-10-15 00:00:00+00'::timestamptz
          FROM litecr.source_record CROSS JOIN (VALUES ('test-v1'),('test-v2')) versions(v);"
    ).execute(pool).await?;
    let before = marker_snapshot(pool).await?;
    let acl_before = parent_acl(pool).await?;
    let migration30 = MIGRATOR
        .iter()
        .find(|m| m.version == 30)
        .expect("partition migration exists");

    // The DDL, copy and schema-meta update execute before a deliberate late SQL
    // failure. Rolling back must restore the original table, data and ACL.
    let mut tx = pool.begin().await?;
    sqlx::raw_sql(&migration30.sql).execute(&mut *tx).await?;
    let injected = sqlx::query("SELECT 1/0")
        .execute(&mut *tx)
        .await
        .expect_err("late failure is injected");
    assert_eq!(
        injected
            .as_database_error()
            .and_then(|error| error.code())
            .as_deref(),
        Some("22012")
    );
    tx.rollback().await?;
    assert_eq!(marker_snapshot(pool).await?, before);
    assert_eq!(parent_acl(pool).await?, acl_before);
    let original: (String, i64, i64) = sqlx::query_as(
        "SELECT c.relkind::text,(SELECT version FROM litecr.schema_meta WHERE singleton),
                (SELECT count(*) FROM pg_inherits WHERE inhparent=c.oid)
         FROM pg_class c WHERE c.oid='matching.indexed_source_version'::regclass",
    )
    .fetch_one(pool)
    .await?;
    assert_eq!(original, ("r".into(), 29, 0));
    let absent: bool =
        sqlx::query_scalar("SELECT to_regclass('matching.indexed_source_version_p0') IS NULL")
            .fetch_one(pool)
            .await?;
    assert!(absent, "rolled-back partition DDL leaves no child behind");
    let ledger30: i64 =
        sqlx::query_scalar("SELECT count(*) FROM public._sqlx_migrations WHERE version>=30")
            .fetch_one(pool)
            .await?;
    assert_eq!(ledger30, 0);

    // Unexpected column ACLs are rejected rather than silently rewritten.
    let mut tx = pool.begin().await?;
    sqlx::query(&format!(
        "GRANT SELECT(source_record_id) ON matching.indexed_source_version TO {worker}"
    ))
    .execute(&mut *tx)
    .await?;
    let rejected = sqlx::raw_sql(&migration30.sql)
        .execute(&mut *tx)
        .await
        .expect_err("unexpected column ACL fails closed");
    assert!(
        rejected
            .to_string()
            .contains("PARTITION_MARKER_UNEXPECTED_DEPENDENCY_OR_POLICY")
    );
    tx.rollback().await?;

    let mut tx = pool.begin().await?;
    sqlx::query(
        "CREATE TABLE matching.synthetic_marker_dependency(
         source_record_id UUID,receive_version BIGINT,normalizer_version TEXT,
         FOREIGN KEY(source_record_id,receive_version,normalizer_version)
           REFERENCES matching.indexed_source_version(source_record_id,receive_version,normalizer_version))",
    ).execute(&mut *tx).await?;
    let rejected = sqlx::raw_sql(&migration30.sql)
        .execute(&mut *tx)
        .await
        .expect_err("unexpected inbound dependency fails closed");
    assert!(
        rejected
            .to_string()
            .contains("PARTITION_MARKER_UNEXPECTED_DEPENDENCY_OR_POLICY")
    );
    tx.rollback().await?;

    let mut tx = pool.begin().await?;
    sqlx::query(
        "CREATE INDEX synthetic_marker_extra_idx ON matching.indexed_source_version(indexed_at)",
    )
    .execute(&mut *tx)
    .await?;
    let rejected = sqlx::raw_sql(&migration30.sql)
        .execute(&mut *tx)
        .await
        .expect_err("unexpected index fails closed instead of disappearing");
    assert!(
        rejected
            .to_string()
            .contains("PARTITION_MARKER_UNEXPECTED_INDEX")
    );
    tx.rollback().await?;

    let mut tx = pool.begin().await?;
    sqlx::query(&format!("SET LOCAL ROLE {runtime}"))
        .execute(&mut *tx)
        .await?;
    sqlx::query(&format!(
        "GRANT SELECT ON matching.indexed_source_version TO {worker}"
    ))
    .execute(&mut *tx)
    .await?;
    sqlx::query("RESET ROLE").execute(&mut *tx).await?;
    let rejected = sqlx::raw_sql(&migration30.sql)
        .execute(&mut *tx)
        .await
        .expect_err("external grant chain is not flattened");
    assert!(
        rejected
            .to_string()
            .contains("PARTITION_MARKER_EXTERNAL_ACL_GRANTOR")
    );
    tx.rollback().await?;

    for (fixture, expected) in [
        (
            "CREATE RULE synthetic_marker_rule AS ON INSERT TO matching.indexed_source_version DO INSTEAD NOTHING",
            "PARTITION_MARKER_UNEXPECTED_DEPENDENCY_OR_POLICY",
        ),
        (
            "ALTER TABLE matching.indexed_source_version REPLICA IDENTITY FULL",
            "PARTITION_MARKER_OWNER_OR_TABLE_POLICY",
        ),
        (
            "CREATE TABLE matching.synthetic_marker_inherits () INHERITS (matching.indexed_source_version)",
            "PARTITION_MARKER_UNEXPECTED_DEPENDENCY_OR_POLICY",
        ),
        (
            "CREATE PUBLICATION synthetic_marker_explicit FOR TABLE matching.indexed_source_version",
            "PARTITION_MARKER_UNEXPECTED_DEPENDENCY_OR_POLICY",
        ),
        (
            "CREATE PUBLICATION synthetic_marker_schema FOR TABLES IN SCHEMA matching",
            "PARTITION_MARKER_UNEXPECTED_DEPENDENCY_OR_POLICY",
        ),
        (
            "CREATE PUBLICATION synthetic_marker_all FOR ALL TABLES",
            "PARTITION_MARKER_UNEXPECTED_DEPENDENCY_OR_POLICY",
        ),
    ] {
        let mut tx = pool.begin().await?;
        sqlx::query(fixture).execute(&mut *tx).await?;
        let rejected = sqlx::raw_sql(&migration30.sql)
            .execute(&mut *tx)
            .await
            .expect_err("custom rule/replication/inheritance policy fails closed");
        assert!(
            rejected.to_string().contains(expected),
            "fixture {fixture}: {rejected}"
        );
        tx.rollback().await?;
    }

    migrate(pool).await?;
    migrate(pool).await?;
    assert_eq!(
        marker_snapshot(pool).await?,
        before,
        "all original marker timestamps/rows survive"
    );
    assert_eq!(
        parent_acl(pool).await?,
        acl_before,
        "effective direct ACL and grant options survive"
    );
    let state: (String, String, i64, i64) = sqlx::query_as(
        "SELECT c.relkind::text,pg_get_partkeydef(c.oid),
                (SELECT count(*) FROM pg_inherits WHERE inhparent=c.oid),
                (SELECT version FROM litecr.schema_meta WHERE singleton)
         FROM pg_class c WHERE c.oid='matching.indexed_source_version'::regclass",
    )
    .fetch_one(pool)
    .await?;
    assert_eq!(
        state,
        (
            "p".into(),
            "HASH (source_record_id)".into(),
            8,
            SCHEMA_VERSION
        )
    );
    let distribution: (i64, i64) = sqlx::query_as(
        "SELECT count(*),count(DISTINCT tableoid) FROM matching.indexed_source_version",
    )
    .fetch_one(pool)
    .await?;
    assert_eq!(distribution.0, 256);
    assert!(
        distribution.1 > 1,
        "native inserts route across hash partitions"
    );
    let valid: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pg_index WHERE indisvalid AND indisready
         AND (indrelid='matching.indexed_source_version'::regclass OR indrelid IN
           (SELECT inhrelid FROM pg_inherits WHERE inhparent='matching.indexed_source_version'::regclass))",
    ).fetch_one(pool).await?;
    assert_eq!(
        valid, 18,
        "two parent and sixteen child indexes are valid/ready"
    );
    let child_rights: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pg_inherits WHERE inhparent='matching.indexed_source_version'::regclass
         AND (has_table_privilege($1,inhrelid,'SELECT') OR has_table_privilege($1,inhrelid,'INSERT')
           OR has_table_privilege($2,inhrelid,'SELECT') OR has_table_privilege($2,inhrelid,'INSERT'))",
    ).bind(runtime).bind(worker).fetch_one(pool).await?;
    assert_eq!(
        child_rights, 0,
        "new child default ACLs do not expose direct access"
    );
    let mut tx = pool.begin().await?;
    sqlx::query(&format!("SET LOCAL ROLE {worker}"))
        .execute(&mut *tx)
        .await?;
    let visible: i64 = sqlx::query_scalar("SELECT count(*) FROM matching.indexed_source_version")
        .fetch_one(&mut *tx)
        .await?;
    assert_eq!(
        visible, 256,
        "parent SELECT routes without direct child privileges"
    );
    tx.rollback().await?;
    assert_role_denied(
        pool,
        worker,
        "SELECT * FROM matching.indexed_source_version_p0",
    )
    .await?;
    assert_role_denied(pool, worker, "INSERT INTO matching.indexed_source_version SELECT * FROM matching.indexed_source_version LIMIT 1").await?;
    assert_role_denied(
        pool,
        runtime,
        "UPDATE matching.indexed_source_version SET indexed_at=now() WHERE FALSE",
    )
    .await?;
    let mut tx = pool.begin().await?;
    sqlx::query(&format!("SET LOCAL ROLE {runtime}"))
        .execute(&mut *tx)
        .await?;
    sqlx::query("INSERT INTO matching.indexed_source_version SELECT * FROM matching.indexed_source_version LIMIT 1 ON CONFLICT DO NOTHING")
        .execute(&mut *tx).await?;
    tx.rollback().await?;
    let duplicate = sqlx::query("INSERT INTO matching.indexed_source_version SELECT * FROM matching.indexed_source_version LIMIT 1")
        .execute(pool).await.expect_err("global logical PK remains enforced");
    assert_eq!(
        duplicate
            .as_database_error()
            .and_then(|error| error.code())
            .as_deref(),
        Some("23505")
    );
    let missing_source = sqlx::query("INSERT INTO matching.indexed_source_version VALUES('ffffffff-ffff-ffff-ffff-ffffffffffff',1,'test-v1',now())")
        .execute(pool).await.expect_err("source version FK remains enforced");
    assert_eq!(
        missing_source
            .as_database_error()
            .and_then(|error| error.code())
            .as_deref(),
        Some("23503")
    );
    let plan: Value = sqlx::query_scalar(
        "EXPLAIN (FORMAT JSON) SELECT * FROM matching.indexed_source_version
         WHERE source_record_id='00000000-0000-0000-0000-000000000001'::uuid AND receive_version=1 AND normalizer_version='test-v1'",
    ).fetch_one(pool).await?;
    let mut leaves = BTreeSet::new();
    collect_marker_leaves(&plan, &mut leaves);
    assert_eq!(
        leaves.len(),
        1,
        "UUID equality prunes to exactly one hash partition"
    );
    let ledger: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM public._sqlx_migrations WHERE version=30 AND success",
    )
    .fetch_one(pool)
    .await?;
    assert_eq!(ledger, 1);
    Ok(())
}

async fn marker_snapshot(pool: &PgPool) -> Result<String, sqlx::Error> {
    sqlx::query_scalar("SELECT jsonb_agg(to_jsonb(t) ORDER BY source_record_id,receive_version,normalizer_version)::text FROM matching.indexed_source_version t")
        .fetch_one(pool).await
}

async fn parent_acl(pool: &PgPool) -> Result<String, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT jsonb_agg(jsonb_build_array(a.grantee,a.privilege_type,a.is_grantable)
          ORDER BY a.grantee,a.privilege_type,a.is_grantable)::text
         FROM pg_class c CROSS JOIN LATERAL aclexplode(COALESCE(c.relacl,acldefault('r',c.relowner))) a
         WHERE c.oid='matching.indexed_source_version'::regclass",
    ).fetch_one(pool).await
}

async fn assert_role_denied(pool: &PgPool, role: &str, statement: &str) -> Result<(), TestError> {
    let mut tx = pool.begin().await?;
    sqlx::query(&format!("SET LOCAL ROLE {role}"))
        .execute(&mut *tx)
        .await?;
    let error = sqlx::query(statement)
        .execute(&mut *tx)
        .await
        .expect_err("role operation must be denied");
    assert_eq!(
        error
            .as_database_error()
            .and_then(|error| error.code())
            .as_deref(),
        Some("42501")
    );
    tx.rollback().await?;
    Ok(())
}

fn collect_marker_leaves(value: &Value, leaves: &mut BTreeSet<String>) {
    match value {
        Value::Object(fields) => {
            if let Some(name) = fields.get("Relation Name").and_then(Value::as_str) {
                if name.starts_with("indexed_source_version_p") {
                    leaves.insert(name.to_owned());
                }
            }
            for child in fields.values() {
                collect_marker_leaves(child, leaves);
            }
        }
        Value::Array(items) => {
            for child in items {
                collect_marker_leaves(child, leaves);
            }
        }
        _ => {}
    }
}
