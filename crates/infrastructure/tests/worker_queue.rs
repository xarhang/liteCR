use litecr_application::{
    access::ActorKind,
    registry::{RegistryActor, RegistryStore},
    worker::{AdvisoryMatchingConsumer, ConsumerCompletion},
};
use litecr_domain::registry::{SourceAssertion, SourceRecordCommand};
use litecr_domain::worker::{ConsumerLease, ConsumerReceiptCode};
use litecr_infrastructure::{
    connect, migrate, registry::PostgresRegistryStore, worker::PostgresMatchingConsumer,
};
use sqlx::{PgPool, Row};
use std::time::Duration;
use tokio::time::{Instant, sleep, timeout};

// These tests claim from one shared synthetic database queue. Serialize them
// so one test cannot lease another test's just-created event before it is
// prioritized or asserted.
static WORKER_QUEUE_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

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
async fn postgres_consumer_receipt_and_evidence_commit_atomically_and_replay_safely() {
    let _test_guard = WORKER_QUEUE_TEST_LOCK.lock().await;
    let url = std::env::var("LITECR_TEST_DATABASE_URL")
        .expect("set LITECR_TEST_DATABASE_URL to an isolated synthetic-only test DB");
    let pool = connect(&url, 12).await.expect("test DB connection");
    assert_synthetic_database(&pool).await;
    migrate(&pool).await.expect("all embedded migrations");
    let key = suffix();
    let (registry, organization, source_id, event_id) = seed(&pool, &key, "1986-04-19").await;
    prioritize_event_for_test(&pool, &event_id).await;
    let consumer = PostgresMatchingConsumer::new(pool.clone());
    let leases = consumer
        .claim(&format!("worker-a-{key}"), 100, 60, 12)
        .await
        .unwrap();
    let lease = leases
        .iter()
        .find(|lease| lease.event_id == event_id)
        .expect("accepted outbox event is enqueued transactionally")
        .clone();
    release_unrelated_leases(&pool, &leases, &event_id).await;

    // A trigger failure after evidence inserts must roll back the run, receipt and job update.
    let trigger = format!("worker_receipt_reject_{}", std::process::id());
    let function = format!("worker_receipt_reject_fn_{}", std::process::id());
    sqlx::query(&format!(
        "CREATE FUNCTION litecr.{function}() RETURNS trigger LANGUAGE plpgsql AS $$ \
         BEGIN RAISE EXCEPTION 'synthetic receipt failure'; END $$"
    ))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        &format!(
            "CREATE TRIGGER {trigger} BEFORE INSERT ON litecr.consumer_receipt \
             FOR EACH ROW WHEN (NEW.event_id = '{event_id}'::uuid) EXECUTE FUNCTION litecr.{function}()"
        ),
    )
    .execute(&pool)
    .await
    .unwrap();
    let failed = consumer.evaluate_and_complete(&lease, 20).await;
    let before_retry = evidence_and_receipt_counts(&pool, &event_id).await;
    assert_eq!(before_retry, (0, 0, "leased".into()));
    sqlx::query(&format!(
        "DROP TRIGGER {trigger} ON litecr.consumer_receipt"
    ))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(&format!("DROP FUNCTION litecr.{function}()"))
        .execute(&pool)
        .await
        .unwrap();
    assert!(failed.is_err());

    let completion = consumer
        .evaluate_and_complete(&lease, 20)
        .await
        .expect("evidence, receipt and job completion commit together");
    let run_id = match completion {
        ConsumerCompletion::EvidencePersisted { evidence_run_id } => evidence_run_id,
        ConsumerCompletion::Obsolete { code } => panic!("unexpected terminal result: {code:?}"),
    };
    assert!(!run_id.is_empty());
    assert_eq!(
        evidence_and_receipt_counts(&pool, &event_id).await,
        (1, 1, "completed".into())
    );

    // Even after a later source update, replay returns the immutable completion receipt.
    let mut later_source = command(&registry, &organization, &key, "query", "1986-04-20");
    later_source.source_version = Some("v2-query".into());
    PostgresRegistryStore::new(pool.clone())
        .ingest(&actor(&key), &format!("later-source-{key}"), &later_source)
        .await
        .expect("later source snapshot");
    assert!(matches!(
        consumer.evaluate_and_complete(&lease, 20).await.unwrap(),
        ConsumerCompletion::EvidencePersisted { evidence_run_id } if evidence_run_id == run_id
    ));
    assert_eq!(
        evidence_and_receipt_counts(&pool, &event_id).await,
        (1, 1, "completed".into())
    );
    cleanup(&pool, &registry, &organization, &source_id, &event_id).await;
    pool.close().await;
}

#[tokio::test]
async fn postgres_consumer_marks_superseded_event_obsolete_without_stale_evidence() {
    let _test_guard = WORKER_QUEUE_TEST_LOCK.lock().await;
    let url = std::env::var("LITECR_TEST_DATABASE_URL")
        .expect("set LITECR_TEST_DATABASE_URL to an isolated synthetic-only test DB");
    let pool = connect(&url, 12).await.expect("test DB connection");
    assert_synthetic_database(&pool).await;
    migrate(&pool).await.expect("all embedded migrations");
    let key = suffix();
    let (registry, organization, source_id, event_id) = seed(&pool, &key, "1986-04-19").await;
    let actor = actor(&key);
    let mut update = command(&registry, &organization, &key, "query", "1986-04-20");
    update.source_version = Some("v2-query".into());
    PostgresRegistryStore::new(pool.clone())
        .ingest(&actor, &format!("update-{key}"), &update)
        .await
        .expect("advance source to a later accepted version");
    prioritize_event_for_test(&pool, &event_id).await;
    let consumer = PostgresMatchingConsumer::new(pool.clone());
    let claimed = consumer
        .claim(&format!("worker-stale-{key}"), 100, 60, 12)
        .await
        .unwrap();
    let lease = claimed
        .iter()
        .find(|lease| lease.event_id == event_id)
        .expect("old event remains independently queued")
        .clone();
    release_unrelated_leases(&pool, &claimed, &event_id).await;
    let completion = consumer.evaluate_and_complete(&lease, 20).await;
    assert!(
        matches!(
            completion,
            Ok(ConsumerCompletion::Obsolete {
                code: ConsumerReceiptCode::SourceSuperseded
            })
        ),
        "unexpected worker completion: {completion:?}"
    );
    assert_eq!(
        evidence_and_receipt_counts(&pool, &event_id).await,
        (0, 1, "obsolete".into())
    );
    cleanup(&pool, &registry, &organization, &source_id, &event_id).await;
    pool.close().await;
}

#[tokio::test]
async fn postgres_consumer_claims_once_and_recovers_expired_lease() {
    let _test_guard = WORKER_QUEUE_TEST_LOCK.lock().await;
    let url = std::env::var("LITECR_TEST_DATABASE_URL")
        .expect("set LITECR_TEST_DATABASE_URL to an isolated synthetic-only test DB");
    let pool = connect(&url, 12).await.expect("test DB connection");
    assert_synthetic_database(&pool).await;
    migrate(&pool).await.expect("all embedded migrations");
    let key = suffix();
    let (registry, organization, source_id, event_id) = seed(&pool, &key, "1986-04-19").await;
    prioritize_event_for_test(&pool, &event_id).await;
    let consumer = PostgresMatchingConsumer::new(pool.clone());
    let owner_a = format!("worker-a-{key}");
    let owner_b = format!("worker-b-{key}");
    let (a, b) = tokio::join!(
        consumer.claim(&owner_a, 100, 60, 12),
        consumer.claim(&owner_b, 100, 60, 12)
    );
    let claimed: Vec<_> = a.unwrap().into_iter().chain(b.unwrap()).collect();
    let target_leases: Vec<_> = claimed
        .iter()
        .filter(|lease| lease.event_id == event_id)
        .collect();
    assert_eq!(
        target_leases.len(),
        1,
        "SKIP LOCKED gives one consumer the event"
    );
    let target_lease = (*target_leases[0]).clone();
    // Other due synthetic jobs may share this fixture. Return only those claimed here
    // to the ready queue so this test does not strand unrelated work.
    release_unrelated_leases(&pool, &claimed, &event_id).await;
    sqlx::query(
        "UPDATE litecr.consumer_job SET lease_expires_at=now()-interval '1 second' \
         WHERE consumer_name='advisory-matcher-v1' AND event_id=$1::uuid",
    )
    .bind(&event_id)
    .execute(&pool)
    .await
    .unwrap();
    let recovered = consumer.claim(&owner_b, 100, 60, 12).await.unwrap();
    let recovered_target = recovered
        .iter()
        .find(|lease| lease.event_id == event_id)
        .expect("expired target lease is reclaimed");
    assert_eq!(recovered_target.lease_owner, owner_b);
    assert_eq!(recovered_target.attempt, target_lease.attempt + 1);
    assert_ne!(recovered_target.lease_token, target_lease.lease_token);
    release_unrelated_leases(&pool, &recovered, &event_id).await;
    assert!(
        consumer
            .retry(recovered_target, "TRANSIENT_DATABASE", 3, 2)
            .await
            .unwrap()
    );
    let delayed: bool = sqlx::query_scalar(
        "SELECT available_at > now() FROM litecr.consumer_job WHERE event_id=$1::uuid",
    )
    .bind(&event_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(delayed, "retry uses a bounded exponential delay");
    // Keep the retry deterministically due even if this host's test scheduler
    // runs the next claim at the same transaction timestamp boundary.
    sqlx::query(
        "UPDATE litecr.consumer_job SET available_at=now()-interval '1 second' WHERE event_id=$1::uuid",
    )
        .bind(&event_id)
        .execute(&pool)
        .await
        .unwrap();
    let final_attempt = consumer.claim(&owner_a, 100, 60, 12).await.unwrap();
    let final_target = final_attempt
        .iter()
        .find(|lease| lease.event_id == event_id)
        .expect("retry is due after the test advances available_at");
    assert_eq!(final_target.attempt, 3);
    release_unrelated_leases(&pool, &final_attempt, &event_id).await;
    assert!(
        !consumer
            .retry(final_target, "EVIDENCE_UNAVAILABLE", 3, 2)
            .await
            .unwrap()
    );
    assert_eq!(
        evidence_and_receipt_counts(&pool, &event_id).await,
        (0, 1, "failed".into())
    );
    cleanup(&pool, &registry, &organization, &source_id, &event_id).await;
    pool.close().await;
}

#[tokio::test]
async fn postgres_consumer_exhausts_expired_lease_at_configured_attempt_limit() {
    let _test_guard = WORKER_QUEUE_TEST_LOCK.lock().await;
    let url = std::env::var("LITECR_TEST_DATABASE_URL")
        .expect("set LITECR_TEST_DATABASE_URL to an isolated synthetic-only test DB");
    let pool = connect(&url, 12).await.expect("test DB connection");
    assert_synthetic_database(&pool).await;
    migrate(&pool).await.expect("all embedded migrations");
    let key = suffix();
    let (registry, organization, source_id, event_id) = seed(&pool, &key, "1986-04-19").await;
    sqlx::query(
        "UPDATE litecr.consumer_job SET state='leased',attempts=3,lease_owner=$2, \
         lease_token=gen_random_uuid(),lease_expires_at=now()-interval '1 second' \
         WHERE consumer_name='advisory-matcher-v1' AND event_id=$1::uuid",
    )
    .bind(&event_id)
    .bind(format!("expired-worker-{key}"))
    .execute(&pool)
    .await
    .unwrap();

    let consumer = PostgresMatchingConsumer::new(pool.clone());
    let claimed = consumer
        .claim(&format!("worker-limit-{key}"), 100, 60, 3)
        .await
        .unwrap();
    assert!(
        claimed.iter().all(|lease| lease.event_id != event_id),
        "an expired lease at the configured maximum must not be reclaimed"
    );
    release_unrelated_leases(&pool, &claimed, &event_id).await;
    let state: String =
        sqlx::query_scalar("SELECT state FROM litecr.consumer_job WHERE event_id=$1::uuid")
            .bind(&event_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    let receipt: (String, String) = sqlx::query_as(
        "SELECT outcome,receipt_code FROM litecr.consumer_receipt WHERE event_id=$1::uuid",
    )
    .bind(&event_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(state, "failed");
    assert_eq!(receipt, ("failed".into(), "RETRY_EXHAUSTED".into()));
    cleanup(&pool, &registry, &organization, &source_id, &event_id).await;
    pool.close().await;
}

#[tokio::test]
async fn postgres_consumer_rechecks_candidate_versions_before_evidence_commit() {
    let _test_guard = WORKER_QUEUE_TEST_LOCK.lock().await;
    let url = std::env::var("LITECR_TEST_DATABASE_URL")
        .expect("set LITECR_TEST_DATABASE_URL to an isolated synthetic-only test DB");
    let pool = connect(&url, 12).await.expect("test DB connection");
    assert_synthetic_database(&pool).await;
    migrate(&pool).await.expect("all embedded migrations");
    let key = suffix();
    let registry = format!("worker-race-reg-{key}");
    let organization = format!("worker-race-org-{key}");
    let actor = actor(&key);
    let registry_store = PostgresRegistryStore::new(pool.clone());
    let query = registry_store
        .ingest(
            &actor,
            &format!("race-query-{key}"),
            &command(&registry, &organization, &key, "query", "1986-04-19"),
        )
        .await
        .unwrap();
    let candidate = registry_store
        .ingest(
            &actor,
            &format!("race-candidate-{key}"),
            &command(&registry, &organization, &key, "candidate", "1986-04-19"),
        )
        .await
        .unwrap();
    let query_event: String = sqlx::query_scalar(
        "SELECT event_id::text FROM litecr.outbox_event WHERE aggregate_id=$1::uuid \
         AND event_type='registry.source_record.accepted'",
    )
    .bind(&query.source_record_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    let candidate_event: String = sqlx::query_scalar(
        "SELECT event_id::text FROM litecr.outbox_event WHERE aggregate_id=$1::uuid \
         AND event_type='registry.source_record.accepted'",
    )
    .bind(&candidate.source_record_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    prioritize_event_for_test(&pool, &query_event).await;
    let consumer = PostgresMatchingConsumer::new(pool.clone());
    let claimed = consumer
        .claim(&format!("worker-race-{key}"), 100, 60, 12)
        .await
        .unwrap();
    let lease = claimed
        .iter()
        .find(|lease| lease.event_id == query_event)
        .unwrap()
        .clone();
    release_unrelated_leases(&pool, &claimed, &query_event).await;
    let mut blocker = pool.begin().await.unwrap();
    let _: String = sqlx::query_scalar(
        "SELECT source_record_id::text FROM litecr.source_record WHERE source_record_id=$1::uuid FOR UPDATE",
    )
    .bind(&candidate.source_record_id)
    .fetch_one(&mut *blocker)
    .await
    .unwrap();
    let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *blocker)
        .await
        .unwrap();
    let matching_consumer = consumer.clone();
    let matching_lease = lease.clone();
    let task = tokio::spawn(async move {
        matching_consumer
            .evaluate_and_complete(&matching_lease, 20)
            .await
    });
    let task = wait_for_worker_lock_wait(&pool, blocker_pid, task).await;
    advance_source_receive_version(&mut blocker, &candidate.source_record_id)
        .await
        .unwrap();
    blocker.commit().await.unwrap();
    let result = timeout(Duration::from_secs(5), task)
        .await
        .expect("worker race result timeout")
        .expect("worker task joined");
    assert!(
        matches!(
            result,
            Err(litecr_application::worker::ConsumerError::StaleSnapshot)
        ),
        "candidate source changed after scoring and must not persist evidence: {result:?}"
    );
    let query_evidence: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM matching.evidence_run WHERE query_source_record_id=$1::uuid",
    )
    .bind(&query.source_record_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(query_evidence, 0);
    cleanup(
        &pool,
        &registry,
        &organization,
        &query.source_record_id,
        &query_event,
    )
    .await;
    cleanup(
        &pool,
        &registry,
        &organization,
        &candidate.source_record_id,
        &candidate_event,
    )
    .await;
    pool.close().await;
}

async fn wait_for_worker_lock_wait(
    pool: &PgPool,
    blocker_pid: i32,
    task: tokio::task::JoinHandle<
        Result<ConsumerCompletion, litecr_application::worker::ConsumerError>,
    >,
) -> tokio::task::JoinHandle<Result<ConsumerCompletion, litecr_application::worker::ConsumerError>>
{
    let deadline = Instant::now() + Duration::from_secs(5);
    let task = task;
    loop {
        assert!(!task.is_finished(), "worker returned before candidate lock");
        let blocked: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM pg_stat_activity a WHERE $1=ANY(pg_blocking_pids(a.pid)) AND a.wait_event_type='Lock')",
        )
        .bind(blocker_pid)
        .fetch_one(pool)
        .await
        .expect("inspect lock wait");
        if blocked {
            return task;
        }
        assert!(
            Instant::now() < deadline,
            "worker did not reach candidate freshness lock"
        );
        sleep(Duration::from_millis(10)).await;
    }
}

async fn advance_source_receive_version(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    source_record_id: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO litecr.source_record_version \
         (source_record_id,receive_version,source_version,payload_hash,version_state) \
         SELECT sr.source_record_id,sr.current_receive_version+1,'synthetic-v2',sv.payload_hash,'accepted' \
         FROM litecr.source_record sr JOIN litecr.source_record_version sv \
           ON sv.source_record_id=sr.source_record_id AND sv.receive_version=sr.current_receive_version \
         WHERE sr.source_record_id=$1::uuid",
    )
    .bind(source_record_id)
    .execute(&mut **tx)
    .await?;
    sqlx::query("UPDATE litecr.source_record SET current_receive_version=current_receive_version+1 WHERE source_record_id=$1::uuid")
        .bind(source_record_id)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

async fn seed(pool: &PgPool, key: &str, dob: &str) -> (String, String, String, String) {
    let registry = format!("worker-reg-{key}");
    let organization = format!("worker-org-{key}");
    let actor = actor(key);
    let command = command(&registry, &organization, key, "query", dob);
    let receipt = PostgresRegistryStore::new(pool.clone())
        .ingest(&actor, &format!("seed-{key}"), &command)
        .await
        .expect("synthetic accepted source record");
    let event_id: String = sqlx::query_scalar(
        "SELECT event_id::text FROM litecr.outbox_event WHERE correlation_id=$1 \
         AND event_type='registry.source_record.accepted'",
    )
    .bind(actor.correlation_id.as_deref().unwrap())
    .fetch_one(pool)
    .await
    .unwrap();
    (registry, organization, receipt.source_record_id, event_id)
}

async fn prioritize_event_for_test(pool: &PgPool, event_id: &str) {
    let rows = sqlx::query(
        "UPDATE litecr.consumer_job SET available_at='epoch'::timestamptz \
         WHERE consumer_name='advisory-matcher-v1' AND event_id=$1::uuid AND state='pending'",
    )
    .bind(event_id)
    .execute(pool)
    .await
    .expect("prioritize only this synthetic test event");
    assert_eq!(
        rows.rows_affected(),
        1,
        "test event is pending exactly once"
    );
}

fn command(
    registry: &str,
    organization: &str,
    key: &str,
    record: &str,
    dob: &str,
) -> SourceRecordCommand {
    SourceRecordCommand {
        registry_id: registry.into(),
        organization_id: organization.into(),
        source_system_id: format!("synthetic-worker-{key}"),
        source_record_key: format!("{record}-{key}"),
        source_version: Some(format!("v1-{record}")),
        assertions: vec![
            SourceAssertion {
                field_path: "name.original".into(),
                original_value: "ສົມໃຈ ທົດສອບ".into(),
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

fn actor(key: &str) -> RegistryActor {
    RegistryActor {
        kind: ActorKind::System,
        id: format!("synthetic-worker-seeder-{key}"),
        issuer_url: None,
        request_id: format!("worker-request-{key}"),
        correlation_id: Some(format!("worker-correlation-{key}")),
    }
}

async fn evidence_and_receipt_counts(pool: &PgPool, event_id: &str) -> (i64, i64, String) {
    let row = sqlx::query(
        "SELECT (SELECT COUNT(*) FROM matching.evidence_run e \
                   JOIN litecr.consumer_receipt r ON r.evidence_run_id=e.run_id \
                   WHERE r.event_id=$1::uuid) AS evidence_count, \
                (SELECT COUNT(*) FROM litecr.consumer_receipt WHERE event_id=$1::uuid) AS receipt_count, \
                (SELECT state FROM litecr.consumer_job WHERE event_id=$1::uuid) AS state",
    )
    .bind(event_id)
    .fetch_one(pool)
    .await
    .unwrap();
    (
        row.try_get("evidence_count").unwrap(),
        row.try_get("receipt_count").unwrap(),
        row.try_get("state").unwrap(),
    )
}

async fn release_unrelated_leases(pool: &PgPool, leases: &[ConsumerLease], target_event: &str) {
    for lease in leases.iter().filter(|lease| lease.event_id != target_event) {
        sqlx::query(
            "UPDATE litecr.consumer_job SET state='pending',attempts=$4,available_at=now(), \
             lease_owner=NULL,lease_token=NULL,lease_expires_at=NULL,updated_at=now() \
             WHERE consumer_name='advisory-matcher-v1' AND event_id=$1::uuid \
               AND state='leased' AND lease_owner=$2 AND lease_token=$3::uuid",
        )
        .bind(&lease.event_id)
        .bind(&lease.lease_owner)
        .bind(&lease.lease_token)
        .bind(lease.attempt - 1)
        .execute(pool)
        .await
        .expect("return only this test's unrelated synthetic claim");
    }
}

async fn cleanup(
    pool: &PgPool,
    registry: &str,
    organization: &str,
    source_id: &str,
    event_id: &str,
) {
    let _ = sqlx::query("DELETE FROM matching.evidence_candidate WHERE run_id IN (SELECT run_id FROM matching.evidence_run WHERE query_source_record_id=$1::uuid)")
        .bind(source_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM matching.evidence_run WHERE query_source_record_id=$1::uuid")
        .bind(source_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM litecr.consumer_receipt WHERE event_id IN (SELECT event_id FROM litecr.outbox_event WHERE aggregate_id=$1::uuid) OR event_id=$2::uuid")
        .bind(source_id)
        .bind(event_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM litecr.consumer_job WHERE source_record_id=$1::uuid")
        .bind(source_id)
        .execute(pool)
        .await;
    let _ = sqlx::query(
        "DELETE FROM litecr.outbox_event WHERE aggregate_id=$1::uuid OR event_id=$2::uuid",
    )
    .bind(source_id)
    .bind(event_id)
    .execute(pool)
    .await;
    let _ = sqlx::query("DELETE FROM litecr.registry_idempotency WHERE source_record_id=$1::uuid")
        .bind(source_id)
        .execute(pool)
        .await;
    let _ = sqlx::query(
        "DELETE FROM litecr.mutation_audit WHERE registry_id=$1 AND organization_id=$2",
    )
    .bind(registry)
    .bind(organization)
    .execute(pool)
    .await;
    let _ = sqlx::query("DELETE FROM litecr.source_identifier_assertion WHERE registry_id=$1 AND organization_id=$2")
        .bind(registry)
        .bind(organization)
        .execute(pool)
        .await;
    let _ =
        sqlx::query("DELETE FROM matching.normalized_assertion WHERE source_record_id=$1::uuid")
            .bind(source_id)
            .execute(pool)
            .await;
    let _ =
        sqlx::query("DELETE FROM matching.normalized_identifier WHERE source_record_id=$1::uuid")
            .bind(source_id)
            .execute(pool)
            .await;
    let _ =
        sqlx::query("DELETE FROM matching.indexed_source_version WHERE source_record_id=$1::uuid")
            .bind(source_id)
            .execute(pool)
            .await;
    let _ = sqlx::query("DELETE FROM litecr.source_assertion WHERE source_record_id=$1::uuid")
        .bind(source_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM litecr.source_record_version WHERE source_record_id=$1::uuid")
        .bind(source_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM litecr.source_record WHERE source_record_id=$1::uuid")
        .bind(source_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM litecr.person WHERE registry_id=$1 AND organization_id=$2")
        .bind(registry)
        .bind(organization)
        .execute(pool)
        .await;
}

async fn assert_synthetic_database(pool: &PgPool) {
    let name: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(pool)
        .await
        .expect("database name");
    let lower = name.to_ascii_lowercase();
    assert!(
        lower.contains("test") || lower.contains("synthetic") || lower.contains("fixture"),
        "refusing to run worker integration tests outside an explicitly synthetic DB: {name}"
    );
    assert_ne!(
        lower, "litecr_access_test",
        "access test DB is out of scope"
    );
}
