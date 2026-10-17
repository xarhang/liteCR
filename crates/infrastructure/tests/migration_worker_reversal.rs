use async_trait::async_trait;
use litecr_application::{
    access::{
        AccessService, AccessServiceConfig, AccessSessionRepository, ActorKind, ActorRef,
        OidcAuthorization, OidcFlow, OidcIdentityClaims, OidcProvider, OidcProviderError,
    },
    migration::{
        MigrationBatch, MigrationRecord, MigrationRun, MigrationService, ReverseManifest,
        ReversePreflightRequest,
    },
    worker::{AdvisoryMatchingConsumer, ConsumerCompletion},
};
use litecr_domain::worker::ConsumerReceiptCode;
use litecr_infrastructure::{
    access::{
        PostgresAccessAuthenticator, PostgresAccessDirectory, PostgresAccessSecurityAudit,
        PostgresAccessSessionRepository, PostgresOidcFlowStore,
    },
    connect, migrate,
    migration::PostgresMigrationExecutor,
    worker::PostgresMatchingConsumer,
};
use serde_json::json;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use std::{sync::Arc, time::SystemTime};

const SYNTHETIC_ISSUER: &str = "https://litecr-migration-worker-test.invalid";

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

fn sha256(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

fn migration_batch(key: &str, registry: &str, organization: &str) -> MigrationBatch {
    let row = json!({
        "source_system_id": format!("synthetic-source-{key}"),
        "source_record_key": format!("worker-source-{key}"),
        "source_version": "source-v1",
        "public_id": format!("worker-public-{key}"),
        "cruid": format!("worker-cruid-{key}"),
        "aliases": [format!("worker-alias-{key}")],
        "assertions": [
            {"field_path":"name.original","original_value":"ຄຳ ທົດສອບ","language_tag":"lo","script":"Lao"},
            {"field_path":"birthDate","original_value":"1984-06","partial_date_precision":"month"}
        ],
        "identifiers": [{"authority":"urn:litecr:test:worker","type":"MRN","value":format!("worker-mrn-{key}"),"authoritative":true}]
    });
    let line = serde_json::to_vec(&row).expect("synthetic source row serializes");
    let mut exact_bytes = line.clone();
    exact_bytes.push(b'\n');
    let record = MigrationRecord {
        source_system_id: row["source_system_id"].as_str().unwrap().to_owned(),
        source_record_key: row["source_record_key"].as_str().unwrap().to_owned(),
        source_version: row["source_version"].as_str().unwrap().to_owned(),
        payload_sha256: sha256(&line),
        raw_ndjson: exact_bytes.clone(),
    };
    MigrationBatch {
        run: MigrationRun {
            manifest_sha256: sha256(format!("manifest-{key}").as_bytes()),
            stream_sha256: sha256(&exact_bytes),
            snapshot_id: format!("synthetic-snapshot-{key}"),
            source_system_id: record.source_system_id.clone(),
            registry_id: registry.to_owned(),
            organization_id: organization.to_owned(),
            expected_record_count: 1,
        },
        sequence: 1,
        first_input_line: 1,
        last_input_line: 1,
        batch_sha256: sha256(&exact_bytes),
        exact_bytes,
        records: vec![record],
    }
}

#[tokio::test]
async fn postgres_migration_reverse_retains_pending_job_and_worker_completes_source_withdrawn() {
    let database_url = std::env::var("LITECR_TEST_DATABASE_URL")
        .expect("set LITECR_TEST_DATABASE_URL to the isolated synthetic litecr_test DB");
    let key = suffix();
    let result = tokio::spawn(run_scenario(database_url.clone(), key.clone())).await;
    cleanup(&database_url, &key).await;
    assert!(
        result.is_ok(),
        "migration/worker scenario task panicked: {result:?}"
    );
    result
        .unwrap()
        .expect("migration and advisory worker scenario");
}

async fn run_scenario(database_url: String, key: String) -> Result<(), String> {
    let pool = connect(&database_url, 8)
        .await
        .map_err(|error| format!("synthetic PostgreSQL connection: {error}"))?;
    let database_name: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(&pool)
        .await
        .map_err(|_| "current database name queried")?;
    ensure(
        database_name == "litecr_test",
        "database guard allows only the named litecr_test fixture",
    )?;
    migrate(&pool)
        .await
        .map_err(|error| format!("embedded migrations apply: {error}"))?;

    let registry = format!("worker-reverse-reg-{key}");
    let organization = format!("worker-reverse-org-{key}");
    let subject = format!("migration-worker-registrar-{key}");
    let access = provisioned_access(&pool, &key, &registry, &organization, &subject).await?;
    let batch = migration_batch(&key, &registry, &organization);
    let manifest_hash = batch.run.manifest_sha256.to_vec();
    let service = MigrationService::new(Box::new(PostgresMigrationExecutor::new(pool.clone())));
    service
        .commit_batch(Some(&access), batch.clone())
        .await
        .map_err(|error| format!("synthetic manifest import commits: {error:?}"))?;

    let source_id: String = sqlx::query_scalar(
        "SELECT imported_source_record_id::text FROM litecr.migration_journal WHERE manifest_sha256=$1",
    )
    .bind(&manifest_hash)
    .fetch_one(&pool)
    .await
    .map_err(|_| "manifest journal yields its imported source id")?;
    let event_id: String = sqlx::query_scalar(
        "SELECT event_id::text FROM litecr.outbox_event WHERE aggregate_id=$1::uuid AND event_type='registry.source_record.accepted'",
    )
    .bind(&source_id)
    .fetch_one(&pool)
    .await
    .map_err(|_| "import emits the accepted source event")?;
    let initial_job: (String, i64) = sqlx::query_as(
        "SELECT state,expected_receive_version FROM litecr.consumer_job WHERE consumer_name='advisory-matcher-v1' AND event_id=$1::uuid",
    )
    .bind(&event_id)
    .fetch_one(&pool)
    .await
    .map_err(|_| "accepted source event is enqueued for matching")?;
    ensure(
        initial_job == ("pending".to_owned(), 1),
        "matching job is pending at the imported receive version before reversal",
    )?;

    let preflight = service
        .reverse_preflight(
            Some(&access),
            &ReversePreflightRequest {
                manifest_sha256: batch.run.manifest_sha256,
                registry_id: registry.clone(),
                organization_id: organization.clone(),
            },
        )
        .await
        .map_err(|error| format!("reverse preflight runs: {error:?}"))?;
    ensure(
        preflight.eligible && preflight.blockers.is_empty(),
        "manifest is eligible for source-only reversal while its advisory job is pending",
    )?;
    let reversed = service
        .reverse_manifest(
            Some(&access),
            ReverseManifest {
                manifest_sha256: batch.run.manifest_sha256,
                registry_id: registry.clone(),
                organization_id: organization.clone(),
                reason: "synthetic pending worker reversal regression".to_owned(),
            },
        )
        .await
        .map_err(|error| format!("source-only manifest reversal commits: {error:?}"))?;
    ensure(
        reversed.withdrawn_record_count == 1,
        "reversal withdraws only the imported source record",
    )?;

    let retained: (i64, i64, String, i64, i64, i64) = sqlx::query_as(
        "SELECT \
            (SELECT count(*) FROM litecr.consumer_job WHERE event_id=$1::uuid), \
            (SELECT count(*) FROM litecr.outbox_event WHERE event_id=$1::uuid AND event_type='registry.source_record.accepted'), \
            (SELECT state FROM litecr.consumer_job WHERE event_id=$1::uuid), \
            (SELECT count(*) FROM litecr.outbox_event WHERE aggregate_id=$2::uuid AND event_type='registry.source_record.withdrawn'), \
            (SELECT count(*) FROM litecr.mutation_audit WHERE resource_ref=$2 AND action='migration.source.withdraw'), \
            (SELECT count(*) FROM litecr.migration_reverse_receipt WHERE manifest_sha256=$3)",
    )
    .bind(&event_id)
    .bind(&source_id)
    .bind(&manifest_hash)
    .fetch_one(&pool)
    .await
    .map_err(|_| "post-reversal event, job, audit, and receipt state queried")?;
    ensure(
        retained == (1, 1, "pending".to_owned(), 1, 1, 1),
        "reversal retains the original pending job and accepted event plus withdrawal event, audit, and receipt",
    )?;

    sqlx::query(
        "UPDATE litecr.consumer_job SET available_at='epoch'::timestamptz WHERE consumer_name='advisory-matcher-v1' AND event_id=$1::uuid AND state='pending'",
    )
    .bind(&event_id)
    .execute(&pool)
    .await
    .map_err(|_| "only the fixture job made immediately due")?;
    let consumer = PostgresMatchingConsumer::new(pool.clone());
    let leases = consumer
        .claim(&format!("withdrawn-worker-{key}"), 100, 60, 12)
        .await
        .map_err(|error| format!("worker claims the retained event: {error:?}"))?;
    let lease = leases
        .iter()
        .find(|lease| lease.event_id == event_id)
        .cloned()
        .ok_or("worker claims the retained source event")?;
    release_unrelated_leases(&pool, &leases, &event_id).await?;
    let completion = consumer
        .evaluate_and_complete(&lease, 20)
        .await
        .map_err(|error| format!("worker completes the withdrawn source event: {error:?}"))?;
    ensure(
        matches!(
            completion,
            ConsumerCompletion::Obsolete {
                code: ConsumerReceiptCode::SourceWithdrawn
            }
        ),
        "worker classifies the retained event as SourceWithdrawn",
    )?;

    let final_state: (String, String, String, i64, i64, i64, i64, i64) = sqlx::query_as(
        "SELECT \
            j.state, r.outcome, r.receipt_code, \
            (SELECT count(*) FROM matching.evidence_run WHERE query_source_record_id=$2::uuid), \
            (SELECT count(*) FROM litecr.outbox_event WHERE event_id=$1::uuid AND event_type='registry.source_record.accepted'), \
            (SELECT count(*) FROM litecr.outbox_event WHERE aggregate_id=$2::uuid AND event_type='registry.source_record.withdrawn'), \
            (SELECT count(*) FROM litecr.mutation_audit WHERE resource_ref=$2 AND action='migration.source.withdraw'), \
            (SELECT count(*) FROM litecr.migration_reverse_receipt WHERE manifest_sha256=$3) \
         FROM litecr.consumer_job j JOIN litecr.consumer_receipt r USING (consumer_name,event_id) WHERE j.event_id=$1::uuid",
    )
    .bind(&event_id)
    .bind(&source_id)
    .bind(&manifest_hash)
    .fetch_one(&pool)
    .await
    .map_err(|_| "terminal worker receipt and no-evidence state queried")?;
    ensure(
        final_state
            == (
                "obsolete".into(),
                "obsolete".into(),
                "SOURCE_WITHDRAWN".into(),
                0,
                1,
                1,
                1,
                1,
            ),
        "worker records SOURCE_WITHDRAWN with no evidence and preserves migration withdrawal provenance",
    )?;
    pool.close().await;
    Ok(())
}

async fn provisioned_access(
    pool: &PgPool,
    key: &str,
    registry: &str,
    organization: &str,
    subject: &str,
) -> Result<litecr_application::access::AccessContext, String> {
    sqlx::query("INSERT INTO litecr.access_identity (actor_kind,issuer_url,external_subject) VALUES ('human',$1,$2)")
        .bind(SYNTHETIC_ISSUER)
        .bind(subject)
        .execute(pool)
        .await
        .map_err(|_| "fixture registrar identity inserted")?;
    sqlx::query("INSERT INTO litecr.access_grant (actor_kind,issuer_url,external_subject,registry_id,organization_id,record_ids,role_name,permission_name) VALUES ('human',$1,$2,$3,$4,NULL,'registrar',NULL)")
        .bind(SYNTHETIC_ISSUER)
        .bind(subject)
        .bind(registry)
        .bind(organization)
        .execute(pool)
        .await
        .map_err(|_| "fixture registrar grant inserted")?;
    sqlx::query("INSERT INTO litecr.access_grant (actor_kind,issuer_url,external_subject,registry_id,organization_id,record_ids,role_name,permission_name) VALUES ('human',$1,$2,$3,$4,NULL,NULL,'migration:write')")
        .bind(SYNTHETIC_ISSUER)
        .bind(subject)
        .bind(registry)
        .bind(organization)
        .execute(pool)
        .await
        .map_err(|_| "fixture migration permission inserted")?;

    let directory = PostgresAccessDirectory::new(pool.clone());
    let sessions = PostgresAccessSessionRepository::new(pool.clone(), directory.clone());
    let actor = ActorRef {
        kind: ActorKind::Human,
        id: subject.to_owned(),
        issuer_url: Some(SYNTHETIC_ISSUER.to_owned()),
    };
    let session = sessions
        .create(
            &actor,
            Some("synthetic-migration-worker-session"),
            600,
            &format!("worker-reversal-session-{key}"),
        )
        .await
        .map_err(|_| "fixture registrar session created")?;
    let service = AccessService::new(
        Arc::new(UnusedOidcProvider),
        Arc::new(PostgresOidcFlowStore::new(pool.clone())),
        Arc::new(sessions.clone()),
        Arc::new(directory),
        Arc::new(PostgresAccessAuthenticator::new(sessions)),
        Arc::new(PostgresAccessSecurityAudit::new(pool.clone())),
        AccessServiceConfig {
            login_flow_lifetime_seconds: 300,
            session_lifetime_seconds: 600,
        },
    )
    .ok_or("fixture AccessService configuration valid")?;
    service
        .authenticate(
            &session.opaque_token,
            Some(&session.csrf_token),
            true,
            &format!("worker-reversal-auth-{key}"),
        )
        .await
        .map_err(|_| "existing authenticator resolves fixture registrar session".to_owned())
}

struct UnusedOidcProvider;

#[async_trait]
impl OidcProvider for UnusedOidcProvider {
    async fn begin_authorization(&self) -> Result<OidcAuthorization, OidcProviderError> {
        Err(OidcProviderError::Unavailable)
    }

    async fn exchange_code(
        &self,
        _code: &str,
        _flow: &OidcFlow,
    ) -> Result<OidcIdentityClaims, OidcProviderError> {
        Err(OidcProviderError::Unavailable)
    }
}

async fn release_unrelated_leases(
    pool: &PgPool,
    leases: &[litecr_domain::worker::ConsumerLease],
    target_event_id: &str,
) -> Result<(), String> {
    for lease in leases
        .iter()
        .filter(|lease| lease.event_id != target_event_id)
    {
        sqlx::query(
            "UPDATE litecr.consumer_job SET state='pending',attempts=GREATEST(attempts-1,0),lease_owner=NULL,lease_token=NULL,lease_expires_at=NULL WHERE consumer_name='advisory-matcher-v1' AND event_id=$1::uuid AND lease_token=$2::uuid",
        )
        .bind(&lease.event_id)
        .bind(&lease.lease_token)
        .execute(pool)
        .await
        .map_err(|_| "unrelated claimed fixture lease released")?;
    }
    Ok(())
}

async fn cleanup(database_url: &str, key: &str) {
    let pool = connect(database_url, 4)
        .await
        .expect("the dedicated synthetic fixture remains available for cleanup");
    let database_name: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(&pool)
        .await
        .expect("cleanup database name");
    assert_eq!(database_name, "litecr_test", "cleanup is fixture-only");
    let registry = format!("worker-reverse-reg-{key}");
    let organization = format!("worker-reverse-org-{key}");
    let subject = format!("migration-worker-registrar-{key}");
    let manifest_hash = sha256(format!("manifest-{key}").as_bytes()).to_vec();
    let source_ids: Vec<String> = sqlx::query_scalar(
        "SELECT source_record_id::text FROM litecr.source_record WHERE registry_id=$1 AND organization_id=$2",
    )
    .bind(&registry)
    .bind(&organization)
    .fetch_all(&pool)
    .await
    .expect("only unique test-owned source IDs selected");
    sqlx::query("DELETE FROM litecr.consumer_receipt WHERE event_id IN (SELECT event_id FROM litecr.outbox_event WHERE registry_id=$1 AND organization_id=$2)")
        .bind(&registry).bind(&organization).execute(&pool).await.expect("fixture consumer receipts removed first");
    sqlx::query("DELETE FROM litecr.consumer_job WHERE registry_id=$1 AND organization_id=$2")
        .bind(&registry)
        .bind(&organization)
        .execute(&pool)
        .await
        .expect("fixture jobs removed before accepted events");
    sqlx::query("DELETE FROM litecr.outbox_event WHERE registry_id=$1 AND organization_id=$2")
        .bind(&registry)
        .bind(&organization)
        .execute(&pool)
        .await
        .expect("fixture outbox events removed");
    sqlx::query("DELETE FROM litecr.migration_reverse_receipt WHERE manifest_sha256=$1")
        .bind(&manifest_hash)
        .execute(&pool)
        .await
        .expect("fixture reverse receipt removed");
    sqlx::query("DELETE FROM litecr.migration_journal WHERE manifest_sha256=$1")
        .bind(&manifest_hash)
        .execute(&pool)
        .await
        .expect("fixture journal removed");
    sqlx::query("DELETE FROM litecr.migration_effect WHERE manifest_sha256=$1")
        .bind(&manifest_hash)
        .execute(&pool)
        .await
        .expect("fixture migration effects removed");
    sqlx::query("DELETE FROM litecr.migration_batch WHERE manifest_sha256=$1")
        .bind(&manifest_hash)
        .execute(&pool)
        .await
        .expect("fixture migration batch removed");
    sqlx::query("DELETE FROM litecr.migration_checkpoint WHERE manifest_sha256=$1")
        .bind(&manifest_hash)
        .execute(&pool)
        .await
        .expect("fixture checkpoint removed");
    sqlx::query("DELETE FROM litecr.registry_idempotency WHERE actor_id=$1 AND registry_id=$2")
        .bind(&subject)
        .bind(&registry)
        .execute(&pool)
        .await
        .expect("fixture idempotency removed");
    sqlx::query("DELETE FROM litecr.mutation_audit WHERE registry_id=$1 AND organization_id=$2")
        .bind(&registry)
        .bind(&organization)
        .execute(&pool)
        .await
        .expect("fixture mutation audits removed");
    if !source_ids.is_empty() {
        sqlx::query("DELETE FROM matching.evidence_candidate WHERE run_id IN (SELECT run_id FROM matching.evidence_run WHERE query_source_record_id::text=ANY($1::text[]))")
            .bind(&source_ids).execute(&pool).await.expect("fixture evidence candidates removed");
        sqlx::query(
            "DELETE FROM matching.evidence_run WHERE query_source_record_id::text=ANY($1::text[])",
        )
        .bind(&source_ids)
        .execute(&pool)
        .await
        .expect("fixture evidence runs removed");
        sqlx::query("DELETE FROM matching.normalized_assertion WHERE source_record_id::text=ANY($1::text[])")
            .bind(&source_ids).execute(&pool).await.expect("fixture normalized assertions removed");
        sqlx::query("DELETE FROM matching.normalized_identifier WHERE source_record_id::text=ANY($1::text[])")
            .bind(&source_ids).execute(&pool).await.expect("fixture normalized identifiers removed");
        sqlx::query("DELETE FROM matching.indexed_source_version WHERE source_record_id::text=ANY($1::text[])")
            .bind(&source_ids).execute(&pool).await.expect("fixture matching markers removed");
        sqlx::query("DELETE FROM litecr.source_identifier_assertion WHERE source_record_id::text=ANY($1::text[])")
            .bind(&source_ids).execute(&pool).await.expect("fixture source identifiers removed");
        sqlx::query(
            "DELETE FROM litecr.person_identifier WHERE source_record_id::text=ANY($1::text[])",
        )
        .bind(&source_ids)
        .execute(&pool)
        .await
        .expect("fixture person identifiers removed");
        sqlx::query(
            "DELETE FROM litecr.source_assertion WHERE source_record_id::text=ANY($1::text[])",
        )
        .bind(&source_ids)
        .execute(&pool)
        .await
        .expect("fixture assertions removed");
        sqlx::query(
            "DELETE FROM litecr.source_record_version WHERE source_record_id::text=ANY($1::text[])",
        )
        .bind(&source_ids)
        .execute(&pool)
        .await
        .expect("fixture source versions removed");
        sqlx::query(
            "DELETE FROM litecr.source_record WHERE source_record_id::text=ANY($1::text[])",
        )
        .bind(&source_ids)
        .execute(&pool)
        .await
        .expect("fixture sources removed");
    }
    let public_id = format!("worker-public-{key}");
    let person_ids: Vec<String> = sqlx::query_scalar("SELECT person_id::text FROM litecr.person WHERE registry_id=$1 AND organization_id=$2 AND public_id=$3")
        .bind(&registry).bind(&organization).bind(&public_id).fetch_all(&pool).await.expect("only exact fixture person selected");
    if !person_ids.is_empty() {
        sqlx::query("DELETE FROM litecr.person_alias WHERE person_id::text=ANY($1::text[])")
            .bind(&person_ids)
            .execute(&pool)
            .await
            .expect("fixture aliases removed");
        sqlx::query("DELETE FROM litecr.person WHERE person_id::text=ANY($1::text[])")
            .bind(&person_ids)
            .execute(&pool)
            .await
            .expect("fixture person removed");
    }
    sqlx::query("DELETE FROM litecr.access_session WHERE actor_kind='human' AND issuer_url=$1 AND external_subject=$2")
        .bind(SYNTHETIC_ISSUER).bind(&subject).execute(&pool).await.expect("fixture session removed");
    sqlx::query("DELETE FROM litecr.access_grant WHERE actor_kind='human' AND issuer_url=$1 AND external_subject=$2")
        .bind(SYNTHETIC_ISSUER).bind(&subject).execute(&pool).await.expect("fixture grants removed");
    sqlx::query("DELETE FROM litecr.access_audit WHERE actor_issuer_url=$1 AND actor_id=$2")
        .bind(SYNTHETIC_ISSUER)
        .bind(&subject)
        .execute(&pool)
        .await
        .expect("fixture access audit removed");
    sqlx::query("DELETE FROM litecr.access_identity WHERE actor_kind='human' AND issuer_url=$1 AND external_subject=$2")
        .bind(SYNTHETIC_ISSUER).bind(&subject).execute(&pool).await.expect("fixture identity removed last");
    pool.close().await;
}

fn ensure(condition: bool, message: &str) -> Result<(), String> {
    condition.then_some(()).ok_or_else(|| message.to_owned())
}
