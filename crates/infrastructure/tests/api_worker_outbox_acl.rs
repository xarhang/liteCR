//! Confirms the API database identity can insert an accepted source and its
//! migration-owned outbox trigger enqueues advisory matching without worker grants.
//! Runtime matching persists complete, immutable historical evidence by INSERT.

use litecr_application::{
    access::ActorKind,
    matching::{MatchIdempotency, MatchingStore},
    registry::{RegistryActor, RegistryStore},
};
use litecr_domain::registry::{SourceAssertion, SourceRecordCommand};
use litecr_infrastructure::{
    connect, matching::PostgresMatchingStore, registry::PostgresRegistryStore,
};

#[tokio::test]
async fn runtime_source_and_match_evidence_work_without_snapshot_update_rights() {
    let Ok(url) = std::env::var("LITECR_API_ROLE_TEST_DATABASE_URL") else {
        eprintln!("skipping API/outbox ACL exercise; isolated role rehearsal not configured");
        return;
    };
    let pool = connect(&url, 4).await.expect("API database connect");
    let current_user: String = sqlx::query_scalar("SELECT current_user")
        .fetch_one(&pool)
        .await
        .expect("database principal");
    assert_eq!(current_user, "litecr_runtime");
    let db: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(&pool)
        .await
        .expect("database name");
    assert!(
        db.to_ascii_lowercase().contains("test") || db.to_ascii_lowercase().contains("synthetic"),
        "refusing non-fixture database: {db}"
    );
    assert_ne!(db.to_ascii_lowercase(), "litecr_access_test");

    let command = SourceRecordCommand {
        registry_id: "synthetic-worker-acl-api".into(),
        organization_id: "synthetic-worker-acl-org".into(),
        source_system_id: "synthetic-worker-acl-api-source".into(),
        source_record_key: format!("runtime-api-{}", std::process::id()),
        source_version: Some("synthetic-v1".into()),
        assertions: vec![
            SourceAssertion {
                field_path: "name.original".into(),
                original_value: "Nguyen Thi Thu synthetic".into(),
                language_tag: Some("vi".into()),
                script: Some("Latin".into()),
            },
            SourceAssertion {
                field_path: "birth_date".into(),
                original_value: "1987-06-12".into(),
                language_tag: None,
                script: None,
            },
        ],
        identifiers: vec![],
    };
    let actor = RegistryActor {
        kind: ActorKind::System,
        id: format!("synthetic-api-role-{}", std::process::id()),
        issuer_url: None,
        request_id: format!("worker-acl-request-{}", std::process::id()),
        correlation_id: Some(format!("worker-acl-correlation-{}", std::process::id())),
    };
    let receipt = PostgresRegistryStore::new(pool.clone())
        .ingest(
            &actor,
            &format!("worker-acl-idem-{}", std::process::id()),
            &command,
        )
        .await
        .expect("runtime can ingest synthetic source");
    let event_id: String = sqlx::query_scalar(
        "SELECT event_id::text FROM litecr.outbox_event WHERE aggregate_id=$1::uuid \
         AND event_type='registry.source_record.accepted' AND correlation_id=$2",
    )
    .bind(&receipt.source_record_id)
    .bind(actor.correlation_id.as_deref().unwrap())
    .fetch_one(&pool)
    .await
    .expect("runtime outbox event");
    let enqueued: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM litecr.consumer_job WHERE event_id=$1::uuid \
         AND consumer_name='advisory-matcher-v1' AND state='pending')",
    )
    .bind(event_id)
    .fetch_one(&pool)
    .await
    .expect("migration trigger enqueue");
    assert!(
        enqueued,
        "outbox trigger should enqueue under API session privileges"
    );
    let mut candidate = command.clone();
    candidate.source_record_key.push_str("-candidate");
    let candidate_receipt = PostgresRegistryStore::new(pool.clone())
        .ingest(
            &actor,
            &format!("worker-acl-candidate-idem-{}", std::process::id()),
            &candidate,
        )
        .await
        .expect("runtime can ingest matching candidate");
    let can_rewrite: bool = sqlx::query_scalar(
        "SELECT has_any_column_privilege(current_user,'matching.evidence_run','UPDATE')",
    )
    .fetch_one(&pool)
    .await
    .expect("runtime snapshot write privilege check");
    assert!(!can_rewrite, "runtime evidence must remain append-only");
    let matcher = PostgresMatchingStore::new(pool.clone());
    let idempotency = Some(MatchIdempotency {
        actor_digest: [0x11; 32],
        key_digest: [0x22; 32],
        request_digest: [0x33; 32],
    });
    let result = matcher
        .evaluate(
            &command.registry_id,
            &command.organization_id,
            &receipt.source_record_id,
            20,
            &actor.id,
            &actor.request_id,
            idempotency,
        )
        .await
        .expect("runtime matching inserts its complete snapshot atomically");
    assert!(result.snapshot_complete);
    assert!(
        result
            .candidates
            .iter()
            .any(|row| row.candidate_record_id == candidate_receipt.source_record_id)
    );
    let run_id = result.evidence_run_id.as_deref().expect("persisted run ID");
    let saved = matcher
        .read_evidence(run_id, &command.registry_id, &command.organization_id)
        .await
        .expect("runtime can read persisted historical evidence");
    assert_eq!(saved, result);
    let replay = matcher
        .evaluate(
            &command.registry_id,
            &command.organization_id,
            &receipt.source_record_id,
            20,
            &actor.id,
            "synthetic-runtime-match-retry",
            idempotency,
        )
        .await
        .expect("runtime replays the exact append-only result");
    assert_eq!(replay, result);
    let rewrite = sqlx::query(
        "UPDATE matching.evidence_run SET result_snapshot='{}'::jsonb WHERE run_id=$1::uuid",
    )
    .bind(run_id)
    .execute(&pool)
    .await
    .expect_err("runtime must not rewrite historical evidence");
    assert_eq!(
        rewrite.as_database_error().and_then(|error| error.code()),
        Some("42501".into())
    );
    assert_eq!(
        matcher
            .read_evidence(run_id, &command.registry_id, &command.organization_id)
            .await
            .expect("denied rewrite preserves saved evidence"),
        result
    );
    pool.close().await;
}
