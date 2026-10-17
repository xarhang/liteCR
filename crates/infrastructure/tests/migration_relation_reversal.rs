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
    registry::{RegistryService, SourceAssertion, SourceRecordCommand},
    review::ReviewService,
};
use litecr_domain::review::{OpenReviewCase, ReviewDecisionCommand, ReviewDecisionKind};
use litecr_infrastructure::{
    access::{
        PostgresAccessAuthenticator, PostgresAccessDirectory, PostgresAccessSecurityAudit,
        PostgresAccessSessionRepository, PostgresOidcFlowStore,
    },
    connect, migrate,
    migration::PostgresMigrationExecutor,
    registry::PostgresRegistryStore,
    review::PostgresReviewStore,
};
use serde_json::json;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use std::{sync::Arc, time::SystemTime};

const SYNTHETIC_ISSUER: &str = "https://litecr-migration-relation-test.invalid";

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

fn batch(key: &str, registry: &str, organization: &str) -> MigrationBatch {
    let row = json!({
        "source_system_id": format!("synthetic-relation-source-{key}"),
        "source_record_key": format!("imported-{key}"),
        "source_version": "source-v1",
        "public_id": format!("relation-imported-{key}"),
        "cruid": format!("relation-cruid-{key}"),
        "aliases": [],
        "assertions": [{"field_path":"name.original","original_value":"Synthetic imported person","language_tag":"en","script":"Latn"}],
        "identifiers": []
    });
    let line = serde_json::to_vec(&row).expect("synthetic row serializes");
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
            manifest_sha256: sha256(format!("relation-manifest-{key}").as_bytes()),
            stream_sha256: sha256(&exact_bytes),
            snapshot_id: format!("synthetic-relation-snapshot-{key}"),
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
async fn postgres_reverse_refuses_imported_source_with_persisted_identity_link_atomically() {
    let url = std::env::var("LITECR_TEST_DATABASE_URL")
        .expect("set LITECR_TEST_DATABASE_URL to isolated synthetic litecr_test DB");
    let key = suffix();
    let result = tokio::spawn(scenario(url.clone(), key.clone())).await;
    cleanup(&url, &key).await;
    assert!(
        result.is_ok(),
        "relation reversal task panicked: {result:?}"
    );
    result
        .unwrap()
        .expect("persisted relation blocks source-only reverse");
}

async fn scenario(url: String, key: String) -> Result<(), String> {
    let pool = connect(&url, 8)
        .await
        .map_err(|error| format!("synthetic PostgreSQL connection: {error}"))?;
    let database: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(&pool)
        .await
        .map_err(|_| "database name queried")?;
    ensure(
        database == "litecr_test",
        "database guard permits only litecr_test",
    )?;
    migrate(&pool)
        .await
        .map_err(|error| format!("embedded migrations apply: {error}"))?;

    let registry = format!("relation-reverse-reg-{key}");
    let organization = format!("relation-reverse-org-{key}");
    let subject = format!("synthetic-relation-reviewer-{key}");
    let context = provision_access(&pool, &key, &registry, &organization, &subject).await?;
    let import = batch(&key, &registry, &organization);
    let manifest = import.run.manifest_sha256.to_vec();
    let migration = MigrationService::new(Box::new(PostgresMigrationExecutor::new(pool.clone())));
    migration
        .commit_batch(Some(&context), import.clone())
        .await
        .map_err(|error| format!("synthetic source import commits: {error:?}"))?;
    let imported_source: String = sqlx::query_scalar(
        "SELECT imported_source_record_id::text FROM litecr.migration_journal WHERE manifest_sha256=$1",
    )
    .bind(&manifest)
    .fetch_one(&pool)
    .await
    .map_err(|_| "manifest journal yields imported source")?;
    let imported_person: String = sqlx::query_scalar(
        "SELECT person_id::text FROM litecr.source_record WHERE source_record_id=$1::uuid",
    )
    .bind(&imported_source)
    .fetch_one(&pool)
    .await
    .map_err(|_| "imported source resolves to its person")?;

    let registry_service =
        RegistryService::new(Box::new(PostgresRegistryStore::new(pool.clone())), 50)
            .ok_or("registry service has a valid page bound")?;
    let candidate = registry_service
        .ingest(
            Some(&context),
            &format!("relation-candidate-ingest-{key}"),
            SourceRecordCommand {
                registry_id: registry.clone(),
                organization_id: organization.clone(),
                source_system_id: format!("synthetic-review-source-{key}"),
                source_record_key: format!("candidate-{key}"),
                source_version: Some("candidate-v1".into()),
                assertions: vec![SourceAssertion {
                    field_path: "name.original".into(),
                    original_value: "Synthetic candidate person".into(),
                    language_tag: Some("en".into()),
                    script: Some("Latn".into()),
                }],
                identifiers: vec![],
            },
        )
        .await
        .map_err(|error| format!("ordinary candidate ingestion: {error:?}"))?;
    let candidate_person: String = sqlx::query_scalar(
        "SELECT person_id::text FROM litecr.source_record WHERE source_record_id=$1::uuid",
    )
    .bind(&candidate.source_record_id)
    .fetch_one(&pool)
    .await
    .map_err(|_| "candidate source resolves to its person")?;

    let review = ReviewService::new(Box::new(PostgresReviewStore::new(pool.clone())));
    let case = review
        .open_case(
            Some(&context),
            OpenReviewCase {
                registry_id: registry.clone(),
                organization_id: organization.clone(),
                query_source_record_id: imported_source.clone(),
                candidate_source_record_id: candidate.source_record_id.clone(),
                evidence_run_id: None,
                expected_query_receive_version: 1,
                expected_candidate_receive_version: 1,
            },
        )
        .await
        .map_err(|error| format!("review case opens through ReviewService: {error:?}"))?;
    let linked = review
        .decide(
            Some(&context),
            &registry,
            &organization,
            ReviewDecisionCommand {
                case_id: case.case_id,
                expected_case_version: case.case_version,
                expected_query_receive_version: 1,
                expected_candidate_receive_version: 1,
                expected_query_person_version: 1,
                expected_candidate_person_version: 1,
                kind: ReviewDecisionKind::Link,
                reason_code: "synthetic_migration_dependency".into(),
                reason: "Synthetic AC-22 persisted-link dependency regression".into(),
                idempotency_key: format!("relation-decision-{key}"),
                merge_id: None,
            },
        )
        .await
        .map_err(|error| format!("identity link decision through ReviewService: {error:?}"))?;
    ensure(
        linked.outcome == "accepted",
        "review service accepts explicit link",
    )?;
    let link: (String, String) = sqlx::query_as(
        "SELECT source_person_id::text,target_person_id::text FROM litecr.identity_link WHERE decision_id=$1::uuid",
    )
    .bind(linked.decision_id.as_deref().ok_or("link decision has durable ID")?)
    .fetch_one(&pool)
    .await
    .map_err(|_| "identity_link persists the service decision")?;
    ensure(
        link.0 == imported_person || link.1 == imported_person,
        "persisted identity link references the imported source person",
    )?;
    ensure(
        (link.0 == imported_person && link.1 == candidate_person)
            || (link.1 == imported_person && link.0 == candidate_person),
        "persisted identity link connects exactly the imported and candidate persons",
    )?;

    let reverse_request = ReverseManifest {
        manifest_sha256: import.run.manifest_sha256,
        registry_id: registry.clone(),
        organization_id: organization.clone(),
        reason: "synthetic reverse blocked by persisted identity link".into(),
    };
    let before = snapshot(&pool, &manifest, &registry, &organization, &imported_source).await?;
    let preflight = migration
        .reverse_preflight(
            Some(&context),
            &ReversePreflightRequest {
                manifest_sha256: import.run.manifest_sha256,
                registry_id: registry.clone(),
                organization_id: organization.clone(),
            },
        )
        .await
        .map_err(|error| format!("reverse preflight returns decision: {error:?}"))?;
    ensure(
        !preflight.eligible,
        "source-only preflight refuses identity dependency",
    )?;
    ensure(
        preflight.blockers.iter().any(|blocker| {
            blocker.code == litecr_application::migration::ReverseBlockerCode::IdentityDependency
        }),
        "preflight reports identity_dependency blocker",
    )?;
    let after_preflight =
        snapshot(&pool, &manifest, &registry, &organization, &imported_source).await?;
    ensure(
        before == after_preflight,
        "read-only preflight leaves all fixture state unchanged",
    )?;

    let reverse = migration
        .reverse_manifest(Some(&context), reverse_request)
        .await;
    ensure(
        matches!(
            reverse,
            Err(litecr_application::migration::MigrationError::ReverseBlocked)
        ),
        "locked transactional reverse recheck refuses persisted identity dependency",
    )?;
    let after_reverse =
        snapshot(&pool, &manifest, &registry, &organization, &imported_source).await?;
    ensure(
        before == after_reverse,
        "refused reverse leaves source, relation and durable state unchanged",
    )?;
    ensure(
        after_reverse.contains("identity_link") && after_reverse.contains("source_record"),
        "state snapshot includes the downstream relation and imported source state",
    )?;
    pool.close().await;
    Ok(())
}

async fn snapshot(
    pool: &PgPool,
    manifest: &[u8],
    registry: &str,
    organization: &str,
    source_id: &str,
) -> Result<String, String> {
    sqlx::query_scalar(
        "SELECT jsonb_build_object(\
            'source_record', (SELECT to_jsonb(s) FROM litecr.source_record s WHERE source_record_id=$4::uuid), \
            'source_versions', COALESCE((SELECT jsonb_agg(to_jsonb(v) ORDER BY receive_version) FROM litecr.source_record_version v WHERE source_record_id=$4::uuid),'[]'::jsonb), \
            'assertions', COALESCE((SELECT jsonb_agg(to_jsonb(a) ORDER BY field_path) FROM litecr.source_assertion a WHERE source_record_id=$4::uuid),'[]'::jsonb), \
            'person', (SELECT to_jsonb(p) FROM litecr.person p WHERE person_id=(SELECT person_id FROM litecr.source_record WHERE source_record_id=$4::uuid)), \
            'identity_links', COALESCE((SELECT jsonb_agg(to_jsonb(l) ORDER BY link_id) FROM litecr.identity_link l WHERE registry_id=$2 AND organization_id=$3 AND (source_person_id=(SELECT person_id FROM litecr.source_record WHERE source_record_id=$4::uuid) OR target_person_id=(SELECT person_id FROM litecr.source_record WHERE source_record_id=$4::uuid))),'[]'::jsonb), \
            'review_cases', COALESCE((SELECT jsonb_agg(to_jsonb(c) ORDER BY case_id) FROM litecr.review_case c WHERE registry_id=$2 AND organization_id=$3),'[]'::jsonb), \
            'checkpoint', (SELECT to_jsonb(c) FROM litecr.migration_checkpoint c WHERE manifest_sha256=$1), \
            'journal', COALESCE((SELECT jsonb_agg(to_jsonb(j) ORDER BY journal_id) FROM litecr.migration_journal j WHERE manifest_sha256=$1),'[]'::jsonb), \
            'effects', COALESCE((SELECT jsonb_agg(to_jsonb(e) ORDER BY effect_id) FROM litecr.migration_effect e WHERE manifest_sha256=$1),'[]'::jsonb), \
            'audit', COALESCE((SELECT jsonb_agg(to_jsonb(a) ORDER BY audit_id) FROM litecr.mutation_audit a WHERE registry_id=$2 AND organization_id=$3),'[]'::jsonb), \
            'outbox', COALESCE((SELECT jsonb_agg(to_jsonb(o) ORDER BY event_id) FROM litecr.outbox_event o WHERE registry_id=$2 AND organization_id=$3),'[]'::jsonb), \
            'reverse_receipts', COALESCE((SELECT jsonb_agg(to_jsonb(r) ORDER BY manifest_sha256) FROM litecr.migration_reverse_receipt r WHERE manifest_sha256=$1),'[]'::jsonb))::text",
    )
    .bind(manifest)
    .bind(registry)
    .bind(organization)
    .bind(source_id)
    .fetch_one(pool)
    .await
    .map_err(|_| "durable source/relation/checkpoint/journal/audit/outbox/receipt snapshot queried".into())
}

async fn provision_access(
    pool: &PgPool,
    key: &str,
    registry: &str,
    organization: &str,
    subject: &str,
) -> Result<litecr_application::access::AccessContext, String> {
    sqlx::query("INSERT INTO litecr.access_identity (actor_kind,issuer_url,external_subject) VALUES ('human',$1,$2)")
        .bind(SYNTHETIC_ISSUER).bind(subject).execute(pool).await.map_err(|_| "synthetic reviewer identity inserted")?;
    for permission in [
        "migration:write",
        "source:write",
        "review:decide",
        "identity:link",
    ] {
        sqlx::query("INSERT INTO litecr.access_grant (actor_kind,issuer_url,external_subject,registry_id,organization_id,record_ids,role_name,permission_name) VALUES ('human',$1,$2,$3,$4,NULL,NULL,$5)")
            .bind(SYNTHETIC_ISSUER).bind(subject).bind(registry).bind(organization).bind(permission).execute(pool).await.map_err(|_| "scoped synthetic permission inserted")?;
    }
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
            Some("synthetic-relation-session"),
            600,
            &format!("relation-session-{key}"),
        )
        .await
        .map_err(|_| "synthetic session created")?;
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
    .ok_or("synthetic AccessService config valid")?;
    service
        .authenticate(
            &session.opaque_token,
            Some(&session.csrf_token),
            true,
            &format!("relation-auth-{key}"),
        )
        .await
        .map_err(|_| "existing authenticator resolves scoped fixture grants".into())
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

async fn cleanup(url: &str, key: &str) {
    let pool = connect(url, 4)
        .await
        .expect("fixture database remains available for cleanup");
    let db: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(&pool)
        .await
        .expect("cleanup database guard");
    assert_eq!(
        db, "litecr_test",
        "cleanup is restricted to isolated test database"
    );
    let registry = format!("relation-reverse-reg-{key}");
    let organization = format!("relation-reverse-org-{key}");
    let subject = format!("synthetic-relation-reviewer-{key}");
    let manifest = sha256(format!("relation-manifest-{key}").as_bytes()).to_vec();
    sqlx::query("DELETE FROM litecr.consumer_receipt WHERE event_id IN (SELECT event_id FROM litecr.outbox_event WHERE registry_id=$1 AND organization_id=$2)").bind(&registry).bind(&organization).execute(&pool).await.expect("fixture receipts removed");
    sqlx::query("DELETE FROM litecr.consumer_job WHERE registry_id=$1 AND organization_id=$2")
        .bind(&registry)
        .bind(&organization)
        .execute(&pool)
        .await
        .expect("fixture jobs removed");
    sqlx::query("DELETE FROM litecr.outbox_event WHERE registry_id=$1 AND organization_id=$2")
        .bind(&registry)
        .bind(&organization)
        .execute(&pool)
        .await
        .expect("fixture outbox removed");
    sqlx::query("DELETE FROM litecr.identity_link WHERE registry_id=$1 AND organization_id=$2")
        .bind(&registry)
        .bind(&organization)
        .execute(&pool)
        .await
        .expect("fixture links removed before decisions");
    sqlx::query("DELETE FROM litecr.review_decision WHERE case_id IN (SELECT case_id FROM litecr.review_case WHERE registry_id=$1 AND organization_id=$2)").bind(&registry).bind(&organization).execute(&pool).await.expect("fixture decisions removed");
    sqlx::query("DELETE FROM litecr.review_case WHERE registry_id=$1 AND organization_id=$2")
        .bind(&registry)
        .bind(&organization)
        .execute(&pool)
        .await
        .expect("fixture review cases removed");
    sqlx::query("DELETE FROM litecr.migration_reverse_receipt WHERE manifest_sha256=$1")
        .bind(&manifest)
        .execute(&pool)
        .await
        .expect("fixture reverse receipts removed");
    sqlx::query("DELETE FROM litecr.migration_journal WHERE manifest_sha256=$1")
        .bind(&manifest)
        .execute(&pool)
        .await
        .expect("fixture journal removed");
    sqlx::query("DELETE FROM litecr.migration_effect WHERE manifest_sha256=$1")
        .bind(&manifest)
        .execute(&pool)
        .await
        .expect("fixture effects removed");
    sqlx::query("DELETE FROM litecr.migration_batch WHERE manifest_sha256=$1")
        .bind(&manifest)
        .execute(&pool)
        .await
        .expect("fixture batch removed");
    sqlx::query("DELETE FROM litecr.migration_checkpoint WHERE manifest_sha256=$1")
        .bind(&manifest)
        .execute(&pool)
        .await
        .expect("fixture checkpoint removed");
    sqlx::query("DELETE FROM litecr.mutation_audit WHERE registry_id=$1 AND organization_id=$2")
        .bind(&registry)
        .bind(&organization)
        .execute(&pool)
        .await
        .expect("fixture audits removed");
    sqlx::query("DELETE FROM litecr.registry_idempotency WHERE registry_id=$1 AND actor_id=$2")
        .bind(&registry)
        .bind(&subject)
        .execute(&pool)
        .await
        .expect("fixture idempotency removed");
    let sources: Vec<String> = sqlx::query_scalar("SELECT source_record_id::text FROM litecr.source_record WHERE registry_id=$1 AND organization_id=$2").bind(&registry).bind(&organization).fetch_all(&pool).await.expect("only unique fixture sources selected");
    if !sources.is_empty() {
        sqlx::query("DELETE FROM matching.evidence_candidate WHERE run_id IN (SELECT run_id FROM matching.evidence_run WHERE query_source_record_id::text=ANY($1::text[]))").bind(&sources).execute(&pool).await.expect("fixture evidence candidates removed");
        sqlx::query(
            "DELETE FROM matching.evidence_run WHERE query_source_record_id::text=ANY($1::text[])",
        )
        .bind(&sources)
        .execute(&pool)
        .await
        .expect("fixture evidence runs removed");
        sqlx::query("DELETE FROM matching.normalized_assertion WHERE source_record_id::text=ANY($1::text[])").bind(&sources).execute(&pool).await.expect("fixture normalized assertions removed");
        sqlx::query("DELETE FROM matching.normalized_identifier WHERE source_record_id::text=ANY($1::text[])").bind(&sources).execute(&pool).await.expect("fixture normalized identifiers removed");
        sqlx::query("DELETE FROM matching.indexed_source_version WHERE source_record_id::text=ANY($1::text[])").bind(&sources).execute(&pool).await.expect("fixture matching markers removed");
        for table in [
            "source_identifier_assertion",
            "source_assertion",
            "source_record_version",
        ] {
            let query =
                format!("DELETE FROM litecr.{table} WHERE source_record_id::text=ANY($1::text[])");
            sqlx::query(&query)
                .bind(&sources)
                .execute(&pool)
                .await
                .expect("fixture source child rows removed");
        }
        sqlx::query(
            "DELETE FROM litecr.source_record WHERE source_record_id::text=ANY($1::text[])",
        )
        .bind(&sources)
        .execute(&pool)
        .await
        .expect("fixture sources removed");
    }
    let people: Vec<String> = sqlx::query_scalar(
        "SELECT person_id::text FROM litecr.person WHERE registry_id=$1 AND organization_id=$2",
    )
    .bind(&registry)
    .bind(&organization)
    .fetch_all(&pool)
    .await
    .expect("only unique fixture people selected");
    if !people.is_empty() {
        sqlx::query("DELETE FROM litecr.person_identifier WHERE person_id::text=ANY($1::text[])")
            .bind(&people)
            .execute(&pool)
            .await
            .expect("fixture person identifiers removed");
        sqlx::query("DELETE FROM litecr.person_alias WHERE person_id::text=ANY($1::text[])")
            .bind(&people)
            .execute(&pool)
            .await
            .expect("fixture aliases removed");
        sqlx::query("DELETE FROM litecr.person WHERE person_id::text=ANY($1::text[])")
            .bind(&people)
            .execute(&pool)
            .await
            .expect("fixture people removed");
    }
    sqlx::query("DELETE FROM litecr.access_session WHERE actor_kind='human' AND issuer_url=$1 AND external_subject=$2").bind(SYNTHETIC_ISSUER).bind(&subject).execute(&pool).await.expect("fixture session removed");
    sqlx::query("DELETE FROM litecr.access_grant WHERE actor_kind='human' AND issuer_url=$1 AND external_subject=$2").bind(SYNTHETIC_ISSUER).bind(&subject).execute(&pool).await.expect("fixture grants removed");
    sqlx::query("DELETE FROM litecr.access_audit WHERE actor_issuer_url=$1 AND actor_id=$2")
        .bind(SYNTHETIC_ISSUER)
        .bind(&subject)
        .execute(&pool)
        .await
        .expect("fixture access audits removed");
    sqlx::query("DELETE FROM litecr.access_identity WHERE actor_kind='human' AND issuer_url=$1 AND external_subject=$2").bind(SYNTHETIC_ISSUER).bind(&subject).execute(&pool).await.expect("fixture identity removed last");
    pool.close().await;
}

fn ensure(condition: bool, message: &str) -> Result<(), String> {
    condition.then_some(()).ok_or_else(|| message.to_owned())
}
