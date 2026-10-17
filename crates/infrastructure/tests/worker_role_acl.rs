//! End-to-end least-privilege worker exercise. The role rehearsal shell script
//! seeds two synthetic queue jobs as the migration owner before running this test.

use litecr_application::worker::{AdvisoryMatchingConsumer, ConsumerCompletion};
use litecr_domain::worker::ConsumerLease;
use litecr_infrastructure::{connect, worker::PostgresMatchingConsumer};
use sqlx::PgPool;

#[tokio::test]
async fn least_privilege_worker_can_claim_evaluate_and_retry_synthetic_jobs() {
    let Ok(url) = std::env::var("LITECR_WORKER_ROLE_TEST_DATABASE_URL") else {
        eprintln!("skipping worker ACL exercise; isolated role rehearsal not configured");
        return;
    };
    let query_event = std::env::var("LITECR_WORKER_QUERY_EVENT_ID").expect("synthetic query event");
    let retry_event = std::env::var("LITECR_WORKER_RETRY_EVENT_ID").expect("synthetic retry event");
    let pool = connect(&url, 8).await.expect("worker database connect");
    assert_fixture_db(&pool).await;
    let current_user: String = sqlx::query_scalar("SELECT current_user")
        .fetch_one(&pool)
        .await
        .expect("database principal");
    assert_eq!(current_user, "litecr_worker");
    let version: i64 = sqlx::query_scalar("SELECT version FROM litecr.schema_meta WHERE singleton")
        .fetch_one(&pool)
        .await
        .expect("read schema version");
    assert_eq!(version, litecr_domain::SCHEMA_VERSION);

    let consumer = PostgresMatchingConsumer::new(pool.clone());
    let claimed = consumer
        .claim("acl-worker-test", 100, 60, 12)
        .await
        .expect("claim queue jobs");
    let query_lease = claimed
        .iter()
        .find(|lease| lease.event_id == query_event)
        .expect("worker can claim query job")
        .clone();
    let retry_lease = claimed
        .iter()
        .find(|lease| lease.event_id == retry_event)
        .expect("worker can claim retry job")
        .clone();
    release_unrelated(
        &pool,
        &claimed,
        &[query_event.as_str(), retry_event.as_str()],
    )
    .await;

    assert!(
        consumer
            .retry(&retry_lease, "TRANSIENT_DATABASE", 3, 1)
            .await
            .expect("worker can retry transient job")
    );
    let retry_state: String =
        sqlx::query_scalar("SELECT state FROM litecr.consumer_job WHERE event_id=$1::uuid")
            .bind(&retry_event)
            .fetch_one(&pool)
            .await
            .expect("read retry state");
    assert_eq!(retry_state, "pending");

    let completion = consumer
        .evaluate_and_complete(&query_lease, 20)
        .await
        .expect("worker can evaluate and persist evidence");
    let evidence_run_id = match completion {
        ConsumerCompletion::EvidencePersisted { evidence_run_id } => evidence_run_id,
        ConsumerCompletion::Obsolete { code } => panic!("unexpected obsolete fixture: {code:?}"),
    };
    let completed: bool = sqlx::query_scalar(
        "SELECT state='completed' FROM litecr.consumer_job WHERE event_id=$1::uuid",
    )
    .bind(&query_event)
    .fetch_one(&pool)
    .await
    .expect("read completion state");
    assert!(completed);
    let receipt_matches: bool = sqlx::query_scalar(
        "SELECT evidence_run_id=$2::uuid FROM litecr.consumer_receipt WHERE event_id=$1::uuid",
    )
    .bind(&query_event)
    .bind(&evidence_run_id)
    .fetch_one(&pool)
    .await
    .expect("read completion receipt");
    assert!(receipt_matches);
    let can_rewrite: bool = sqlx::query_scalar(
        "SELECT has_column_privilege(current_user,'matching.evidence_run','result_snapshot','UPDATE')",
    )
    .fetch_one(&pool)
    .await
    .expect("worker snapshot rewrite privilege check");
    assert!(!can_rewrite, "current worker snapshots are append-only");
    let rewrite = sqlx::query(
        "UPDATE matching.evidence_run SET result_snapshot='{}'::jsonb WHERE run_id=$1::uuid",
    )
    .bind(&evidence_run_id)
    .execute(&pool)
    .await
    .expect_err("worker cannot rewrite completed historical evidence");
    assert_eq!(
        rewrite.as_database_error().and_then(|error| error.code()),
        Some("42501".into())
    );
    pool.close().await;
}

async fn release_unrelated(pool: &PgPool, leases: &[ConsumerLease], fixture_events: &[&str]) {
    for lease in leases
        .iter()
        .filter(|l| !fixture_events.contains(&l.event_id.as_str()))
    {
        sqlx::query(
            "UPDATE litecr.consumer_job SET state='pending',attempts=attempts-1,available_at=now(), \
             lease_owner=NULL,lease_token=NULL,lease_expires_at=NULL,updated_at=now() \
             WHERE consumer_name='advisory-matcher-v1' AND event_id=$1::uuid AND state='leased' \
               AND lease_owner=$2 AND lease_token=$3::uuid",
        ).bind(&lease.event_id).bind(&lease.lease_owner).bind(&lease.lease_token)
            .execute(pool).await.expect("release only this rehearsal's unrelated claim");
    }
}

async fn assert_fixture_db(pool: &PgPool) {
    let db: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(pool)
        .await
        .expect("database name");
    let lower = db.to_ascii_lowercase();
    assert!(
        lower.contains("test") || lower.contains("synthetic") || lower.contains("fixture"),
        "refusing non-fixture DB: {db}"
    );
    assert_ne!(lower, "litecr_access_test", "access test DB is forbidden");
}
