use litecr_application::{
    access::ActorKind,
    registry::{RegistryActor, RegistryStore},
    worker::{AdvisoryMatchingConsumer, ConsumerCompletion},
};
use litecr_domain::registry::{SourceAssertion, SourceRecordCommand};
use litecr_infrastructure::{
    connect, migrate, registry::PostgresRegistryStore, schema_version,
    worker::PostgresMatchingConsumer,
};
use sqlx::{PgPool, postgres::PgPoolOptions};
use std::{
    io::Read,
    process::{Child, Command, ExitStatus, Stdio},
    thread,
    time::{Duration, Instant, SystemTime},
};

fn suffix() -> String {
    format!(
        "{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("system clock after epoch")
            .as_nanos()
    )
}

#[tokio::test]
#[ignore = "requires a dedicated schema-22 fixture; run tools/operations/test-schema22-worker-role-upgrade.sh"]
async fn schema_22_pending_and_expired_leased_jobs_survive_upgrade_and_complete() {
    let database_url = std::env::var("LITECR_TEST_DATABASE_URL")
        .expect("set LITECR_TEST_DATABASE_URL to an isolated synthetic litecr_test DB");
    let key = suffix();
    let joined = tokio::spawn(run_scenario(database_url.clone(), key.clone())).await;
    cleanup(&database_url, &key).await;
    assert!(
        joined.is_ok(),
        "migration queue scenario panicked: {joined:?}"
    );
    joined.unwrap().expect("schema upgrade queue scenario");
}

async fn run_scenario(database_url: String, key: String) -> Result<(), String> {
    let mut pool = connect(&database_url, 8)
        .await
        .map_err(|error| format!("connect isolated PostgreSQL: {error}"))?;
    let database_name: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(&pool)
        .await
        .map_err(|_| "query current database")?;
    ensure(
        database_name == "litecr_test",
        "database is exactly litecr_test",
    )?;
    let initial_version = schema_version(&pool)
        .await
        .map_err(|error| format!("read starting schema version: {error}"))?
        .map(|version| version.value());
    ensure(initial_version == Some(22), "fixture starts at schema 22")?;

    let registry = format!("upgrade-queue-reg-{key}");
    let organization = format!("upgrade-queue-org-{key}");
    let query = seed(
        &pool,
        &registry,
        &format!("{organization}-pending"),
        &key,
        "query",
    )
    .await?;
    let candidate = seed(&pool, &registry, &organization, &key, "candidate").await?;

    sqlx::query(
        "UPDATE litecr.consumer_job SET state='leased',attempts=1,lease_owner=$2, \
         lease_token=gen_random_uuid(),lease_expires_at='epoch'::timestamptz, \
         available_at=now()+interval '5 seconds' \
         WHERE consumer_name='advisory-matcher-v1' AND event_id=$1::uuid AND state='pending'",
    )
    .bind(&candidate.event_id)
    .bind(format!("pre-upgrade-worker-{key}"))
    .execute(&pool)
    .await
    .map_err(|_| "mark only the synthetic candidate job as an expired lease")?;

    let before_pending = job_snapshot(&pool, &query.event_id).await?;
    let before_leased = job_snapshot(&pool, &candidate.event_id).await?;
    ensure(
        before_pending.state == "pending"
            && before_pending.attempts == 0
            && before_pending.lease_token.is_none(),
        "one synthetic job is pending before upgrade",
    )?;
    ensure(
        before_leased.state == "leased"
            && before_leased.attempts == 1
            && before_leased.lease_owner.as_deref()
                == Some(format!("pre-upgrade-worker-{key}").as_str())
            && before_leased.lease_token.is_some()
            && before_leased.lease_expired,
        "one synthetic job has an expired lease before upgrade",
    )?;
    ensure(
        receipt_and_evidence_counts(&pool, &query.event_id).await? == (0, 0)
            && receipt_and_evidence_counts(&pool, &candidate.event_id).await? == (0, 0),
        "both pre-upgrade jobs have no receipt or evidence",
    )?;

    let packaged_binary = std::env::var("LITECR_SCHEMA23_PACKAGE_BINARY").ok();
    if let Some(binary) = &packaged_binary {
        pool.close().await;
        run_packaged_command(
            binary,
            "migrate",
            "LITECR_MIGRATION_DATABASE_URL",
            &database_url,
        )?;
        pool = connect(&database_url, 8)
            .await
            .map_err(|error| format!("reconnect after packaged schema migration: {error}"))?;
    } else {
        migrate(&pool)
            .await
            .map_err(|error| format!("apply current embedded schema upgrade: {error}"))?;
    }
    let upgraded_version = schema_version(&pool)
        .await
        .map_err(|error| format!("read upgraded schema version: {error}"))?
        .map(|version| version.value());
    let expected_version = if packaged_binary.is_some() {
        23
    } else {
        litecr_domain::SCHEMA_VERSION
    };
    ensure(
        upgraded_version == Some(expected_version),
        "migration reaches the package or current-source schema version",
    )?;
    ensure(
        job_snapshot(&pool, &query.event_id).await? == before_pending,
        "schema migration preserves the pending job row",
    )?;
    ensure(
        job_snapshot(&pool, &candidate.event_id).await? == before_leased,
        "schema migration preserves the expired lease row and token",
    )?;

    let worker_database_url = std::env::var("LITECR_WORKER_DATABASE_URL")
        .map_err(|_| "set LITECR_WORKER_DATABASE_URL to the restricted worker login")?;
    let worker_pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&worker_database_url)
        .await
        .map_err(|error| format!("connect using restricted worker credentials: {error}"))?;
    let worker_name: String = sqlx::query_scalar("SELECT current_user")
        .fetch_one(&worker_pool)
        .await
        .map_err(|_| "query worker login")?;
    ensure(
        worker_name == "litecr_worker",
        "queue processing uses litecr_worker login",
    )?;
    ensure(
        query_is_denied(&worker_pool, "SELECT public_id FROM litecr.person LIMIT 1").await,
        "worker cannot read person identity rows",
    )?;
    ensure(
        query_is_denied(
            &worker_pool,
            "UPDATE litecr.consumer_job SET registry_id=registry_id WHERE FALSE",
        )
        .await,
        "worker cannot update immutable queue scope columns",
    )?;

    if let Some(binary) = packaged_binary {
        run_packaged_worker_upgrade_path(PackagedWorkerUpgrade {
            pool: &pool,
            worker_pool: &worker_pool,
            binary: &binary,
            worker_database_url: &worker_database_url,
            query: &query,
            candidate: &candidate,
            before_leased: &before_leased,
            key: &key,
        })
        .await?;
    } else {
        run_source_worker_upgrade_path(
            &pool,
            &worker_pool,
            &query,
            &candidate,
            &before_leased,
            &key,
        )
        .await?;
    }
    worker_pool.close().await;
    pool.close().await;
    Ok(())
}

async fn run_source_worker_upgrade_path(
    pool: &PgPool,
    worker_pool: &PgPool,
    query: &Seeded,
    candidate: &Seeded,
    before_leased: &JobSnapshot,
    key: &str,
) -> Result<(), String> {
    let consumer = PostgresMatchingConsumer::new(worker_pool.clone());
    let owner = format!("post-upgrade-worker-{key}");
    let claimed = consumer
        .claim(&owner, 10, 60, 12)
        .await
        .map_err(|error| format!("claim pre-upgrade queue work after migration: {error:?}"))?;
    let pending_lease = claimed
        .iter()
        .find(|lease| lease.event_id == query.event_id)
        .ok_or("worker claims the pre-upgrade pending job")?;
    let recovered_lease = claimed
        .iter()
        .find(|lease| lease.event_id == candidate.event_id)
        .ok_or("worker reclaims the pre-upgrade expired lease")?;
    ensure(
        pending_lease.attempt == 1 && pending_lease.lease_owner == owner,
        "pending job receives its first post-upgrade lease",
    )?;
    ensure(
        recovered_lease.attempt == 2
            && recovered_lease.lease_owner == owner
            && Some(recovered_lease.lease_token.as_str()) != before_leased.lease_token.as_deref(),
        "expired pre-upgrade lease is recovered with a new token and incremented attempt",
    )?;

    for lease in [pending_lease, recovered_lease] {
        let completion = consumer
            .evaluate_and_complete(lease, 20)
            .await
            .map_err(|error| format!("complete post-upgrade synthetic job: {error:?}"))?;
        ensure(
            matches!(completion, ConsumerCompletion::EvidencePersisted { .. }),
            "valid pre-upgrade source produces matching evidence after schema upgrade",
        )?;
        ensure_completed_once(pool, &lease.event_id).await?;
    }
    Ok(())
}

struct PackagedWorkerUpgrade<'a> {
    pool: &'a PgPool,
    worker_pool: &'a PgPool,
    binary: &'a str,
    worker_database_url: &'a str,
    query: &'a Seeded,
    candidate: &'a Seeded,
    before_leased: &'a JobSnapshot,
    key: &'a str,
}

async fn run_packaged_worker_upgrade_path(input: PackagedWorkerUpgrade<'_>) -> Result<(), String> {
    let PackagedWorkerUpgrade {
        pool,
        worker_pool,
        binary,
        worker_database_url,
        query,
        candidate,
        before_leased,
        key,
    } = input;
    ensure(
        query_is_denied(worker_pool, "SELECT public_id FROM litecr.person LIMIT 1").await,
        "packaged worker role cannot read identity rows",
    )?;
    ensure(
        query_is_denied(
            worker_pool,
            "UPDATE litecr.consumer_job SET registry_id=registry_id WHERE FALSE",
        )
        .await,
        "packaged worker role cannot change queue scope",
    )?;

    let mut blocker = pool
        .begin()
        .await
        .map_err(|_| "open synthetic source-row lock for worker drain")?;
    sqlx::query("SELECT source_record_id FROM litecr.source_record WHERE source_record_id=$1::uuid FOR UPDATE")
        .bind(&candidate.source_record_id)
        .fetch_one(&mut *blocker)
        .await
        .map_err(|_| "hold only the synthetic candidate source row")?;

    let mut worker = PackagedWorker::spawn(binary, worker_database_url, key)?;
    let deadline = Instant::now() + Duration::from_secs(30);
    let recovered: JobSnapshot;
    loop {
        if let Some((status, stderr)) = worker.try_wait()? {
            return Err(format!(
                "packaged worker exited before taking the expired lease: {status}; output: {}",
                stderr.trim()
            ));
        }
        let pending = job_snapshot(pool, &query.event_id).await?;
        let lease = job_snapshot(pool, &candidate.event_id).await?;
        let waiting_on_lock = package_worker_waiting_on_source_lock(worker_pool).await?;
        if pending.state == "completed"
            && pending.attempts == 1
            && lease.state == "leased"
            && lease.attempts == 2
            && !lease.lease_expired
            && waiting_on_lock
        {
            recovered = lease;
            break;
        }
        if Instant::now() >= deadline {
            return Err("packaged worker did not claim both pre-upgrade queue states".to_owned());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    ensure(
        recovered.lease_token.is_some()
            && recovered.lease_token != before_leased.lease_token
            && recovered
                .lease_owner
                .as_deref()
                .is_some_and(|owner| owner.starts_with(&format!("schema23-package-{key}-"))),
        "packaged worker reclaims the expired lease with a new token and owner",
    )?;
    ensure(
        receipt_and_evidence_counts(pool, &query.event_id).await? == (1, 1),
        "first pre-upgrade job has exactly one receipt and evidence run",
    )?;

    // The expired job is deliberately blocked after its lease commits. SIGTERM
    // must let that in-flight package-worker batch finish before it exits.
    let signal = Command::new("kill")
        .arg("-TERM")
        .arg(worker.pid().to_string())
        .status()
        .map_err(|_| "send SIGTERM to packaged worker")?;
    ensure(
        signal.success(),
        "SIGTERM reaches the packaged worker process",
    )?;
    blocker
        .rollback()
        .await
        .map_err(|_| "release synthetic source-row lock after SIGTERM")?;
    let status = worker.wait_until(Duration::from_secs(30))?;
    ensure(
        status.success(),
        "packaged worker drains its leased job and exits successfully on SIGTERM",
    )?;
    ensure_completed_once(pool, &query.event_id).await?;
    ensure_completed_once(pool, &candidate.event_id).await?;
    let completed = job_snapshot(pool, &candidate.event_id).await?;
    ensure(
        completed.state == "completed" && completed.attempts == 2,
        "expired pre-upgrade job completes on attempt two",
    )?;
    Ok(())
}

async fn package_worker_waiting_on_source_lock(pool: &PgPool) -> Result<bool, String> {
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pg_stat_activity WHERE pid<>pg_backend_pid() \
         AND usename=current_user AND wait_event_type='Lock' \
         AND query LIKE '%lock_advisory_source_snapshot%'",
    )
    .fetch_one(pool)
    .await
    .map_err(|_| "inspect the worker's same-role source-lock wait")?;
    Ok(count > 0)
}

fn run_packaged_command(
    binary: &str,
    command: &str,
    database_env: &str,
    database_url: &str,
) -> Result<(), String> {
    let output = Command::new(binary)
        .arg(command)
        .env(database_env, database_url)
        .output()
        .map_err(|_| format!("run exact package {command} command"))?;
    ensure(
        output.status.success(),
        &format!("exact package {command} command exits successfully"),
    )?;
    Ok(())
}

struct PackagedWorker(Option<Child>);

impl PackagedWorker {
    fn spawn(binary: &str, database_url: &str, key: &str) -> Result<Self, String> {
        let child = Command::new(binary)
            .arg("worker")
            .env("LITECR_WORKER_DATABASE_URL", database_url)
            .env("LITECR_WORKER_ID", format!("schema23-package-{key}"))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|_| "start exact schema-23 package worker")?;
        Ok(Self(Some(child)))
    }

    fn pid(&self) -> u32 {
        self.0.as_ref().expect("worker child exists").id()
    }

    fn try_wait(&mut self) -> Result<Option<(ExitStatus, String)>, String> {
        let child = self.0.as_mut().expect("worker child exists");
        let Some(status) = child
            .try_wait()
            .map_err(|_| "check packaged worker process status".to_owned())?
        else {
            return Ok(None);
        };
        let mut stderr = String::new();
        if let Some(mut pipe) = child.stderr.take() {
            pipe.read_to_string(&mut stderr)
                .map_err(|_| "read packaged worker stderr")?;
        }
        if let Some(mut pipe) = child.stdout.take() {
            pipe.read_to_string(&mut stderr)
                .map_err(|_| "read packaged worker output")?;
        }
        Ok(Some((status, stderr)))
    }

    fn wait_until(&mut self, timeout: Duration) -> Result<ExitStatus, String> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some((status, _stderr)) = self.try_wait()? {
                return Ok(status);
            }
            if Instant::now() >= deadline {
                return Err("packaged worker did not exit within its drain timeout".to_owned());
            }
            thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for PackagedWorker {
    fn drop(&mut self) {
        if let Some(child) = self.0.as_mut() {
            if child.try_wait().ok().flatten().is_none() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }
}

async fn ensure_completed_once(pool: &PgPool, event_id: &str) -> Result<(), String> {
    let state: String = sqlx::query_scalar(
        "SELECT state FROM litecr.consumer_job WHERE consumer_name='advisory-matcher-v1' AND event_id=$1::uuid",
    )
    .bind(event_id)
    .fetch_one(pool)
    .await
    .map_err(|_| "query completed package-worker job state")?;
    ensure(state == "completed", "package-worker job is completed")?;
    ensure(
        receipt_and_evidence_counts(pool, event_id).await? == (1, 1),
        "package-worker job has exactly one receipt and one evidence run",
    )?;
    Ok(())
}

async fn query_is_denied(pool: &PgPool, statement: &str) -> bool {
    matches!(
        sqlx::query(statement).execute(pool).await,
        Err(sqlx::Error::Database(error)) if error.code().as_deref() == Some("42501")
    )
}

async fn seed(
    pool: &PgPool,
    registry: &str,
    organization: &str,
    key: &str,
    record: &str,
) -> Result<Seeded, String> {
    let correlation = format!("upgrade-queue-{record}-{key}");
    let command = SourceRecordCommand {
        registry_id: registry.to_owned(),
        organization_id: organization.to_owned(),
        source_system_id: format!("upgrade-queue-source-{key}"),
        source_record_key: format!("{record}-{key}"),
        source_version: Some("pre-upgrade-v1".to_owned()),
        assertions: vec![
            SourceAssertion {
                field_path: "name.original".to_owned(),
                original_value: format!("Synthetic {record} Person"),
                language_tag: Some("en".to_owned()),
                script: Some("Latin".to_owned()),
            },
            SourceAssertion {
                field_path: "birth_date".to_owned(),
                original_value: "1986-04-19".to_owned(),
                language_tag: None,
                script: None,
            },
        ],
        identifiers: vec![],
    };
    let actor = RegistryActor {
        kind: ActorKind::System,
        id: format!("upgrade-queue-seeder-{key}"),
        issuer_url: None,
        request_id: format!("upgrade-queue-request-{record}-{key}"),
        correlation_id: Some(correlation.clone()),
    };
    let receipt = PostgresRegistryStore::new(pool.clone())
        .ingest(
            &actor,
            &format!("upgrade-queue-idempotency-{record}-{key}"),
            &command,
        )
        .await
        .map_err(|error| format!("seed synthetic source before upgrade: {error:?}"))?;
    let event_id: String = sqlx::query_scalar(
        "SELECT event_id::text FROM litecr.outbox_event WHERE correlation_id=$1 AND event_type='registry.source_record.accepted'",
    )
    .bind(correlation)
    .fetch_one(pool)
    .await
    .map_err(|_| "synthetic source emits accepted outbox event")?;
    Ok(Seeded {
        event_id,
        source_record_id: receipt.source_record_id,
    })
}

#[derive(Debug, PartialEq, Eq)]
struct JobSnapshot {
    state: String,
    attempts: i32,
    lease_owner: Option<String>,
    lease_token: Option<String>,
    lease_expired: bool,
}

struct Seeded {
    event_id: String,
    source_record_id: String,
}

async fn job_snapshot(pool: &PgPool, event_id: &str) -> Result<JobSnapshot, String> {
    sqlx::query_as::<_, (String, i32, Option<String>, Option<String>, bool)>(
        "SELECT state,attempts,lease_owner,lease_token::text, \
         COALESCE(lease_expires_at <= now(),FALSE) FROM litecr.consumer_job \
         WHERE consumer_name='advisory-matcher-v1' AND event_id=$1::uuid",
    )
    .bind(event_id)
    .fetch_one(pool)
    .await
    .map(
        |(state, attempts, lease_owner, lease_token, lease_expired)| JobSnapshot {
            state,
            attempts,
            lease_owner,
            lease_token,
            lease_expired,
        },
    )
    .map_err(|_| "read synthetic job state".to_owned())
}

async fn receipt_and_evidence_counts(pool: &PgPool, event_id: &str) -> Result<(i64, i64), String> {
    sqlx::query_as(
        "SELECT (SELECT count(*) FROM litecr.consumer_receipt WHERE event_id=$1::uuid), \
         (SELECT count(*) FROM matching.evidence_run e JOIN litecr.consumer_receipt r \
          ON r.evidence_run_id=e.run_id WHERE r.event_id=$1::uuid)",
    )
    .bind(event_id)
    .fetch_one(pool)
    .await
    .map_err(|_| "count fixture receipts and evidence runs".to_owned())
}

async fn cleanup(database_url: &str, key: &str) {
    let Ok(pool) = connect(database_url, 4).await else {
        return;
    };
    let database_name: Result<String, _> = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(&pool)
        .await;
    if !matches!(database_name.as_deref(), Ok("litecr_test")) {
        pool.close().await;
        return;
    }
    let sources: Vec<String> = sqlx::query_scalar(
        "SELECT source_record_id::text FROM litecr.source_record \
         WHERE registry_id=$1 AND organization_id=ANY($2::text[])",
    )
    .bind(format!("upgrade-queue-reg-{key}"))
    .bind(vec![
        format!("upgrade-queue-org-{key}"),
        format!("upgrade-queue-org-{key}-pending"),
    ])
    .fetch_all(&pool)
    .await
    .unwrap_or_default();
    let _ = sqlx::query(
        "DELETE FROM litecr.consumer_receipt WHERE event_id IN \
         (SELECT event_id FROM litecr.outbox_event WHERE correlation_id LIKE $1)",
    )
    .bind(format!("upgrade-queue-%-{key}"))
    .execute(&pool)
    .await;
    for source_id in &sources {
        let _ = sqlx::query(
            "DELETE FROM matching.evidence_candidate WHERE run_id IN \
             (SELECT run_id FROM matching.evidence_run WHERE query_source_record_id=$1::uuid \
              OR candidate_source_record_id=$1::uuid)",
        )
        .bind(source_id)
        .execute(&pool)
        .await;
    }
    let _ = sqlx::query(
        "DELETE FROM matching.evidence_run WHERE query_source_record_id=ANY($1::uuid[]) \
         OR candidate_source_record_id=ANY($1::uuid[])",
    )
    .bind(&sources)
    .execute(&pool)
    .await;
    let _ = sqlx::query(
        "DELETE FROM litecr.consumer_job WHERE event_id IN \
         (SELECT event_id FROM litecr.outbox_event WHERE correlation_id LIKE $1)",
    )
    .bind(format!("upgrade-queue-%-{key}"))
    .execute(&pool)
    .await;
    let _ = sqlx::query("DELETE FROM litecr.outbox_event WHERE correlation_id LIKE $1")
        .bind(format!("upgrade-queue-%-{key}"))
        .execute(&pool)
        .await;
    for source_id in &sources {
        let _ =
            sqlx::query("DELETE FROM litecr.registry_idempotency WHERE source_record_id=$1::uuid")
                .bind(source_id)
                .execute(&pool)
                .await;
        let _ = sqlx::query(
            "DELETE FROM litecr.source_identifier_assertion WHERE source_record_id=$1::uuid",
        )
        .bind(source_id)
        .execute(&pool)
        .await;
        let _ = sqlx::query(
            "DELETE FROM matching.normalized_assertion WHERE source_record_id=$1::uuid",
        )
        .bind(source_id)
        .execute(&pool)
        .await;
        let _ = sqlx::query(
            "DELETE FROM matching.normalized_identifier WHERE source_record_id=$1::uuid",
        )
        .bind(source_id)
        .execute(&pool)
        .await;
        let _ = sqlx::query("DELETE FROM litecr.source_assertion WHERE source_record_id=$1::uuid")
            .bind(source_id)
            .execute(&pool)
            .await;
        let _ =
            sqlx::query("DELETE FROM litecr.source_record_version WHERE source_record_id=$1::uuid")
                .bind(source_id)
                .execute(&pool)
                .await;
        let _ = sqlx::query("DELETE FROM litecr.source_record WHERE source_record_id=$1::uuid")
            .bind(source_id)
            .execute(&pool)
            .await;
    }
    let _ = sqlx::query(
        "DELETE FROM litecr.person WHERE registry_id=$1 AND organization_id=ANY($2::text[])",
    )
    .bind(format!("upgrade-queue-reg-{key}"))
    .bind(vec![
        format!("upgrade-queue-org-{key}"),
        format!("upgrade-queue-org-{key}-pending"),
    ])
    .execute(&pool)
    .await;
    let _ = sqlx::query(
        "DELETE FROM litecr.mutation_audit WHERE registry_id=$1 AND organization_id=ANY($2::text[])",
    )
    .bind(format!("upgrade-queue-reg-{key}"))
    .bind(vec![
        format!("upgrade-queue-org-{key}"),
        format!("upgrade-queue-org-{key}-pending"),
    ])
    .execute(&pool)
    .await;
    pool.close().await;
}

fn ensure(condition: bool, description: &str) -> Result<(), String> {
    if condition {
        Ok(())
    } else {
        Err(description.to_owned())
    }
}
