use litecr_application::{
    access::ActorKind,
    registry::{RegistryActor, RegistryStore},
};
use litecr_domain::registry::{
    IdentifierAssertion, IngestionOutcome, SourceAssertion, SourceRecordCommand,
};
use litecr_infrastructure::{connect, migrate, registry::PostgresRegistryStore};
use sqlx::Row;

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
async fn postgres_registry_scopes_provenance_conflicts_idempotency_and_outbox() {
    let database_url = std::env::var("LITECR_TEST_DATABASE_URL").expect(
        "set LITECR_TEST_DATABASE_URL to the dedicated synthetic-only litecr_access_test DB",
    );
    let key = suffix();
    let result = tokio::spawn(run_scenario(database_url.clone(), key.clone())).await;
    if std::env::var("LITECR_TEST_ASSUME_MIGRATED").as_deref() != Ok("1") {
        cleanup(&database_url, &key).await;
    }
    assert!(result.is_ok(), "registry scenario task failed: {result:?}");
    result.unwrap().expect("registry PostgreSQL scenario");
}

async fn run_scenario(database_url: String, key: String) -> Result<(), String> {
    let pool = connect(&database_url, 5)
        .await
        .map_err(|_| "dedicated test database connection failed".to_owned())?;
    // The database-role rehearsal migrates as `litecr_migrator`, then runs this
    // same application CRUD scenario as the restricted `litecr_runtime` role.
    // That database is already at the binary's exact schema before this test.
    if std::env::var("LITECR_TEST_ASSUME_MIGRATED").as_deref() != Ok("1") {
        migrate(&pool)
            .await
            .map_err(|_| "dedicated test database migrations failed".to_owned())?;
    }
    let store = PostgresRegistryStore::new(pool.clone());
    let registry = format!("registry-test-{key}");
    let organization = format!("organization-test-{key}");
    let actor = RegistryActor {
        kind: ActorKind::Human,
        id: format!("synthetic-registrar-{key}"),
        issuer_url: Some("https://synthetic-idp.example.test".into()),
        request_id: format!("request-{key}"),
        correlation_id: Some(format!("correlation-{key}")),
    };
    let command = source_command(&registry, &organization, &key, "opaque-v1", "1986-04");
    let idem = format!("synthetic-idempotency-{key}");

    let first = store
        .ingest(&actor, &idem, &command)
        .await
        .map_err(debug_error)?;
    ensure(
        first.outcome == IngestionOutcome::Accepted && !first.replayed,
        "first ingestion should commit",
    )?;
    let retry = store
        .ingest(&actor, &idem, &command)
        .await
        .map_err(debug_error)?;
    ensure(
        retry
            == litecr_domain::registry::IngestionReceipt {
                replayed: true,
                ..first.clone()
            },
        "same key and command should replay",
    )?;

    // The browser may have submitted a committed request but lost its response.
    // The current registration UI creates a new idempotency key on resubmit, so
    // verify the store's exact-source-version fallback returns the same receipt
    // without repeating identity, source-version, audit, or outbox effects.
    let effects_before_retry = ingest_effect_counts(
        &pool,
        &registry,
        &organization,
        &command.source_system_id,
        &command.source_record_key,
        &actor.id,
        actor.correlation_id.as_deref().unwrap(),
    )
    .await?;
    let uncertain_retry = store
        .ingest(
            &actor,
            &format!("synthetic-lost-response-retry-{key}"),
            &command,
        )
        .await
        .map_err(debug_error)?;
    ensure(
        uncertain_retry
            == litecr_domain::registry::IngestionReceipt {
                replayed: true,
                ..first.clone()
            },
        "retry with a fresh key after a lost response should replay the committed receipt",
    )?;
    let effects_after_retry = ingest_effect_counts(
        &pool,
        &registry,
        &organization,
        &command.source_system_id,
        &command.source_record_key,
        &actor.id,
        actor.correlation_id.as_deref().unwrap(),
    )
    .await?;
    ensure(
        effects_after_retry == effects_before_retry,
        "lost-response retry must not duplicate identity, source version, audit, or outbox effects",
    )?;

    let identifier = &command.identifiers[0];
    let found = store
        .search_identifier(
            &registry,
            &organization,
            &identifier.authority,
            &identifier.value,
            10,
        )
        .await
        .map_err(debug_error)?;
    ensure(
        found.len() == 1
            && found[0].public_id == first.person_public_id
            && found[0].identifiers.iter().any(|item| {
                item.authority == identifier.authority
                    && item.identifier_type == identifier.identifier_type
                    && item.value == identifier.value
                    && item.authoritative
            }),
        "exact assigning-authority/value lookup should return the scoped identity projection",
    )?;
    let mut another_namespace = source_command(
        &registry,
        &organization,
        &format!("other-namespace-{key}"),
        "opaque-other-namespace-v1",
        "1988-02",
    );
    another_namespace.identifiers[0].authority = format!("urn:synthetic:other:{key}");
    another_namespace.identifiers[0].identifier_type = "synthetic-other-type".into();
    another_namespace.identifiers[0].value = identifier.value.clone();
    let other_namespace_person = store
        .ingest(
            &actor,
            &format!("synthetic-other-namespace-idempotency-{key}"),
            &another_namespace,
        )
        .await
        .map_err(debug_error)?;
    let value_only = store
        .search_identifier_value(&registry, &organization, &identifier.value, 10)
        .await
        .map_err(debug_error)?;
    ensure(
        value_only.len() == 2
            && value_only
                .iter()
                .any(|person| person.public_id == first.person_public_id)
            && value_only
                .iter()
                .any(|person| person.public_id == other_namespace_person.person_public_id),
        "value-only lookup should search exact values across identifier namespaces in the bound scope",
    )?;
    ensure(
        store
            .search_identifier(
                &registry,
                &format!("other-org-{key}"),
                &identifier.authority,
                &identifier.value,
                10,
            )
            .await
            .map_err(debug_error)?
            .is_empty(),
        "identifier lookup must not expose another organization",
    )?;
    ensure(
        store
            .search_identifier_value(
                &registry,
                &format!("other-org-{key}"),
                &identifier.value,
                10,
            )
            .await
            .map_err(debug_error)?
            .is_empty(),
        "value-only identifier lookup must not expose another organization",
    )?;

    let mut changed_command = command.clone();
    changed_command.assertions[1].original_value = "1986-04-15".into();
    ensure(
        matches!(
            store.ingest(&actor, &idem, &changed_command).await,
            Err(litecr_application::registry::RegistryStoreError::IdempotencyKeyReused)
        ),
        "reused idempotency key must reject a changed payload",
    )?;

    let concurrent_key = format!("concurrent-idem-{key}");
    let (left, right) = tokio::join!(
        store.ingest(&actor, &concurrent_key, &command),
        store.ingest(&actor, &concurrent_key, &command),
    );
    ensure(
        left.is_ok() && right.is_ok(),
        "concurrent same-source ingest should serialize and succeed",
    )?;
    let concurrent_a = left.map_err(debug_error)?;
    let concurrent_b = right.map_err(debug_error)?;
    ensure(
        concurrent_a.source_record_id == first.source_record_id
            && concurrent_b.source_record_id == first.source_record_id,
        "concurrent retries should resolve the same source record",
    )?;
    ensure(
        concurrent_a.replayed && concurrent_b.replayed,
        "concurrent same-key retries should both resolve the committed replay",
    )?;

    let fresh_command = source_command(
        &registry,
        &organization,
        &format!("fresh-{key}"),
        "opaque-fresh-v1",
        "1986-07",
    );
    let fresh_key = format!("fresh-concurrent-idem-{key}");
    let (fresh_left, fresh_right) = tokio::join!(
        store.ingest(&actor, &fresh_key, &fresh_command),
        store.ingest(&actor, &fresh_key, &fresh_command),
    );
    let fresh_left = fresh_left.map_err(debug_error)?;
    let fresh_right = fresh_right.map_err(debug_error)?;
    ensure(
        fresh_left.source_record_id == fresh_right.source_record_id,
        "concurrent first-create requests should share one source record",
    )?;
    ensure(
        fresh_left.person_public_id == fresh_right.person_public_id,
        "concurrent first-create requests should share one person ID",
    )?;
    ensure(
        fresh_left.replayed != fresh_right.replayed,
        "exactly one concurrent first-create request should be the replay",
    )?;

    let scoped = store
        .person(&registry, &organization, &first.person_public_id)
        .await
        .map_err(debug_error)?;
    let scoped = scoped.ok_or_else(|| "scoped person detail was not found".to_owned())?;
    ensure(
        scoped
            .assertions
            .iter()
            .any(|a| a.original_value == "ບຸນມີ" && a.script.as_deref() == Some("Lao")),
        "original Lao-script assertion should be preserved",
    )?;
    ensure(
        scoped
            .assertions
            .iter()
            .any(|a| a.field_path == "birth_date" && a.original_value == "1986-04"),
        "partial birth date precision should remain unchanged",
    )?;
    ensure(
        store
            .person(
                &registry,
                &format!("other-org-{key}"),
                &first.person_public_id,
            )
            .await
            .map_err(debug_error)?
            .is_none(),
        "person must not resolve outside organization scope",
    )?;
    ensure(
        store
            .search_public_id(
                &registry,
                &format!("other-org-{key}"),
                &first.person_public_id,
                10,
            )
            .await
            .map_err(debug_error)?
            .is_empty(),
        "search must not leak person across organization scope",
    )?;

    let version_conflict = source_command(&registry, &organization, &key, "opaque-v1", "1986-05");
    let version_conflict_receipt = store
        .ingest(
            &actor,
            &format!("version-conflict-{key}"),
            &version_conflict,
        )
        .await
        .map_err(debug_error)?;
    ensure(
        version_conflict_receipt.outcome == IngestionOutcome::SourceConflict,
        "changed payload with equal opaque source version should be quarantined",
    )?;
    let conflict_retry = store
        .ingest(
            &actor,
            &format!("version-conflict-retry-{key}"),
            &version_conflict,
        )
        .await
        .map_err(debug_error)?;
    ensure(
        conflict_retry.outcome == IngestionOutcome::SourceConflict && conflict_retry.replayed,
        "exact retry of a quarantined source conflict should replay its conflict outcome",
    )?;
    let next_version = source_command(&registry, &organization, &key, "opaque-v2", "1986-06");
    let accepted_update = store
        .ingest(&actor, &format!("new-version-{key}"), &next_version)
        .await
        .map_err(debug_error)?;
    ensure(
        accepted_update.outcome == IngestionOutcome::Accepted
            && accepted_update.source_receive_version
                > version_conflict_receipt.source_receive_version,
        "a new opaque source version should append after a conflict",
    )?;

    let collision = SourceRecordCommand {
        registry_id: registry.clone(),
        organization_id: organization.clone(),
        source_system_id: format!("other-source-{key}"),
        source_record_key: format!("other-source-key-{key}"),
        source_version: Some("opaque-v1".into()),
        assertions: vec![SourceAssertion {
            field_path: "name.original".into(),
            original_value: "ສົມມະນີ ທົດລອງ".into(),
            language_tag: Some("lo".into()),
            script: Some("Lao".into()),
        }],
        identifiers: vec![IdentifierAssertion {
            authority: format!("urn:synthetic:authority:{key}"),
            identifier_type: "demo-national".into(),
            value: format!("id-{key}"),
            authoritative: true,
        }],
    };
    // Seed an authority assertion on the first source, then submit the same authority/value
    // through another source. The clash is preserved and reported, never reassigned or merged.
    let mut seed_identifier = command.clone();
    seed_identifier.source_version = Some("opaque-v3".into());
    seed_identifier.identifiers = collision.identifiers.clone();
    store
        .ingest(&actor, &format!("seed-identifier-{key}"), &seed_identifier)
        .await
        .map_err(debug_error)?;
    let collision_receipt = store
        .ingest(&actor, &format!("identifier-collision-{key}"), &collision)
        .await
        .map_err(debug_error)?;
    ensure(
        collision_receipt.outcome == IngestionOutcome::SourceConflict,
        "authoritative namespace collision should be quarantined",
    )?;

    let counts = sqlx::query(
        "SELECT (SELECT COUNT(*) FROM litecr.registry_idempotency WHERE actor_id = $1) AS idempotency_count, \
         (SELECT COUNT(*) FROM litecr.mutation_audit WHERE request_id = $2) AS audit_count, \
         (SELECT COUNT(*) FROM litecr.outbox_event WHERE correlation_id = $3) AS outbox_count, \
         (SELECT COUNT(*) FROM litecr.source_identifier_assertion WHERE organization_id = $4 AND assertion_state = 'conflict') AS identifier_conflicts",
    )
    .bind(&actor.id)
    .bind(&actor.request_id)
    .bind(&actor.correlation_id)
    .bind(&organization)
    .fetch_one(&pool)
    .await
    .map_err(|_| "could not inspect exact synthetic evidence counts".to_owned())?;
    let idem_count: i64 = counts
        .try_get("idempotency_count")
        .map_err(|_| "idempotency result unavailable".to_owned())?;
    let audit_count: i64 = counts
        .try_get("audit_count")
        .map_err(|_| "audit result unavailable".to_owned())?;
    let outbox_count: i64 = counts
        .try_get("outbox_count")
        .map_err(|_| "outbox result unavailable".to_owned())?;
    let identifier_conflicts: i64 = counts
        .try_get("identifier_conflicts")
        .map_err(|_| "identifier conflict result unavailable".to_owned())?;
    ensure(
        idem_count == 10,
        "every distinct committed command should have an idempotency outcome",
    )?;
    ensure(
        audit_count == 7,
        "retries should not duplicate mutation audit",
    )?;
    ensure(
        outbox_count == 7,
        "retries should not duplicate outbox effects",
    )?;
    ensure(
        identifier_conflicts >= 1,
        "the duplicate identifier assertion must be retained as conflict",
    )?;

    let event_types = sqlx::query_scalar::<_, String>(
        "SELECT event_type FROM litecr.outbox_event WHERE correlation_id = $1 ORDER BY occurred_at, event_type",
    )
    .bind(&actor.correlation_id)
    .fetch_all(&pool)
    .await
    .map_err(|_| "outbox event type read failed".to_owned())?;
    ensure(
        event_types
            .iter()
            .any(|event| event == "registry.source_record.accepted"),
        "accepted source record should create canonical event",
    )?;
    ensure(
        event_types
            .iter()
            .any(|event| event == "registry.source_record.conflict"),
        "conflict should create distinct catalog event",
    )?;
    let outbox_payloads = sqlx::query_scalar::<_, String>(
        "SELECT payload::text FROM litecr.outbox_event WHERE correlation_id = $1",
    )
    .bind(&actor.correlation_id)
    .fetch_all(&pool)
    .await
    .map_err(|_| "outbox payload read failed".to_owned())?;
    ensure(
        outbox_payloads
            .iter()
            .all(|payload| !payload.contains("ບຸນມີ") && !payload.contains("1986-04")),
        "outbox payloads must exclude original demographics",
    )?;
    // The historical graph fixture requires removing synthetic review rows;
    // exercise it as the test owner, never broaden runtime DELETE privileges.
    if std::env::var("LITECR_TEST_ASSUME_MIGRATED").as_deref() != Ok("1") {
        verify_reverse_alias_projection(
            &pool,
            &store,
            &registry,
            &organization,
            &first.person_public_id,
            &other_namespace_person.person_public_id,
            &key,
        )
        .await?;
    }
    Ok(())
}

// Construct a deliberately cyclic historical redirect graph. This is a read
// projection regression fixture, not a valid sequence of merge commands.
async fn verify_reverse_alias_projection(
    pool: &sqlx::PgPool,
    store: &PostgresRegistryStore,
    registry: &str,
    organization: &str,
    first_public_id: &str,
    target_public_id: &str,
    key: &str,
) -> Result<(), String> {
    let mut tx = pool.begin().await.map_err(debug_error)?;
    let first_id: String =
        sqlx::query_scalar("SELECT person_id::text FROM litecr.person WHERE public_id=$1")
            .bind(first_public_id)
            .fetch_one(&mut *tx)
            .await
            .map_err(debug_error)?;
    let target_id: String =
        sqlx::query_scalar("SELECT person_id::text FROM litecr.person WHERE public_id=$1")
            .bind(target_public_id)
            .fetch_one(&mut *tx)
            .await
            .map_err(debug_error)?;
    let middle_public_id = format!("alias-middle-{key}");
    let inactive_public_id = format!("alias-inactive-{key}");
    let foreign_public_id = format!("alias-foreign-{key}");
    let foreign_org = format!("alias-foreign-org-{key}");
    let mut fixture_people = Vec::new();
    for (public_id, org) in [
        (&middle_public_id, organization),
        (&inactive_public_id, organization),
        (&foreign_public_id, foreign_org.as_str()),
    ] {
        let id: String = sqlx::query_scalar("INSERT INTO litecr.person(public_id,registry_id,organization_id) VALUES($1,$2,$3) RETURNING person_id::text")
            .bind(public_id).bind(registry).bind(org).fetch_one(&mut *tx).await.map_err(debug_error)?;
        sqlx::query("INSERT INTO litecr.person_alias(registry_id,organization_id,alias_value,person_id) VALUES($1,$2,$3,$4::uuid)")
            .bind(registry).bind(org).bind(public_id).bind(&id).execute(&mut *tx).await.map_err(debug_error)?;
        fixture_people.push(id);
    }
    let case_id: String = sqlx::query_scalar("INSERT INTO litecr.review_case(registry_id,organization_id,query_source_record_id,candidate_source_record_id,query_receive_version,candidate_receive_version,created_by) SELECT $1,$2,q.source_record_id,c.source_record_id,q.current_receive_version,c.current_receive_version,$3 FROM litecr.source_record q JOIN litecr.source_record c ON c.person_id=$5::uuid WHERE q.person_id=$4::uuid LIMIT 1 RETURNING case_id::text")
        .bind(registry).bind(organization).bind(key).bind(&first_id).bind(&target_id).fetch_one(&mut *tx).await.map_err(debug_error)?;
    for (position, (source, target, active)) in [
        (&first_id, &fixture_people[0], true),
        (&fixture_people[0], &target_id, true),
        (&target_id, &fixture_people[0], true),
        (&fixture_people[1], &target_id, false),
    ]
    .into_iter()
    .enumerate()
    {
        let decision_id: String = sqlx::query_scalar("INSERT INTO litecr.review_decision(case_id,case_version,decision,actor_id,reason_code,reason,idempotency_key_hash,command_fingerprint,outcome) VALUES($1::uuid,1,'merge',$2,'synthetic','synthetic alias projection fixture',decode(md5($3),'hex') || decode(md5($3),'hex'),decode(md5($3),'hex') || decode(md5($3),'hex'),'accepted') RETURNING decision_id::text")
            .bind(&case_id).bind(key).bind(format!("{key}-{position}")).fetch_one(&mut *tx).await.map_err(debug_error)?;
        let merge_id: String = sqlx::query_scalar("INSERT INTO litecr.identity_merge(registry_id,organization_id,source_person_id,target_person_id,decision_id,source_version_before,target_version_before,source_version_after,target_version_after) VALUES($1,$2,$3::uuid,$4::uuid,$5::uuid,1,1,2,2) RETURNING merge_id::text")
            .bind(registry).bind(organization).bind(source).bind(target).bind(decision_id).fetch_one(&mut *tx).await.map_err(debug_error)?;
        sqlx::query("INSERT INTO litecr.person_redirect(registry_id,organization_id,source_person_id,target_person_id,merge_id,active) VALUES($1,$2,$3::uuid,$4::uuid,$5::uuid,$6)")
            .bind(registry).bind(organization).bind(source).bind(target).bind(merge_id).bind(active).execute(&mut *tx).await.map_err(debug_error)?;
    }
    tx.commit().await.map_err(debug_error)?;
    let result = async {
        let person = store.person(registry, organization, target_public_id).await.map_err(debug_error)?
            .ok_or_else(|| "target of cyclic alias fixture not visible".to_owned())?;
        let mut expected = vec![first_public_id.to_owned(), target_public_id.to_owned(), middle_public_id];
        expected.sort();
        ensure(person.public_id == target_public_id && person.aliases == expected,
            "reverse alias projection must include multi-hop aliases once, sort them, terminate cycles, and exclude inactive/foreign aliases")?;
        ensure(store.person(registry, &foreign_org, target_public_id).await.map_err(debug_error)?.is_none(),
            "alias projection must not cross organization scope")
    }.await;
    // Remove graph fixtures even when an assertion fails, leaving the existing
    // scenario's ordinary registry cleanup unchanged.
    let mut tx = pool.begin().await.map_err(debug_error)?;
    sqlx::query("DELETE FROM litecr.person_redirect WHERE merge_id IN (SELECT merge_id FROM litecr.identity_merge WHERE decision_id IN (SELECT decision_id FROM litecr.review_decision WHERE case_id=$1::uuid))")
        .bind(&case_id).execute(&mut *tx).await.map_err(debug_error)?;
    sqlx::query("DELETE FROM litecr.identity_merge WHERE decision_id IN (SELECT decision_id FROM litecr.review_decision WHERE case_id=$1::uuid)")
        .bind(&case_id).execute(&mut *tx).await.map_err(debug_error)?;
    sqlx::query("DELETE FROM litecr.review_decision WHERE case_id=$1::uuid")
        .bind(&case_id)
        .execute(&mut *tx)
        .await
        .map_err(debug_error)?;
    sqlx::query("DELETE FROM litecr.review_case WHERE case_id=$1::uuid")
        .bind(&case_id)
        .execute(&mut *tx)
        .await
        .map_err(debug_error)?;
    for id in fixture_people {
        sqlx::query("DELETE FROM litecr.person_alias WHERE person_id=$1::uuid")
            .bind(&id)
            .execute(&mut *tx)
            .await
            .map_err(debug_error)?;
        sqlx::query("DELETE FROM litecr.person WHERE person_id=$1::uuid")
            .bind(&id)
            .execute(&mut *tx)
            .await
            .map_err(debug_error)?;
    }
    tx.commit().await.map_err(debug_error)?;
    result
}

fn source_command(
    registry: &str,
    organization: &str,
    suffix: &str,
    version: &str,
    birth_date: &str,
) -> SourceRecordCommand {
    SourceRecordCommand {
        registry_id: registry.into(),
        organization_id: organization.into(),
        source_system_id: format!("source-{suffix}"),
        source_record_key: format!("source-key-{suffix}"),
        source_version: Some(version.into()),
        assertions: vec![
            SourceAssertion {
                field_path: "name.original".into(),
                original_value: "ບຸນມີ".into(),
                language_tag: Some("lo".into()),
                script: Some("Lao".into()),
            },
            SourceAssertion {
                field_path: "birth_date".into(),
                original_value: birth_date.into(),
                language_tag: None,
                script: None,
            },
        ],
        identifiers: vec![IdentifierAssertion {
            authority: format!("urn:synthetic:authority:{suffix}"),
            identifier_type: "demo-national".into(),
            value: format!("id-{suffix}"),
            authoritative: true,
        }],
    }
}

async fn ingest_effect_counts(
    pool: &sqlx::PgPool,
    registry: &str,
    organization: &str,
    source_system: &str,
    source_record_key: &str,
    actor_id: &str,
    correlation_id: &str,
) -> Result<(i64, i64, i64, i64, i64), String> {
    sqlx::query_as::<_, (i64, i64, i64, i64, i64)>(
        "SELECT \
         (SELECT COUNT(*) FROM litecr.person WHERE registry_id=$1 AND organization_id=$2), \
         (SELECT COUNT(*) FROM litecr.source_record WHERE registry_id=$1 AND organization_id=$2 AND source_system_id=$3 AND source_record_key=$4), \
         (SELECT COUNT(*) FROM litecr.source_record_version sv JOIN litecr.source_record sr USING (source_record_id) WHERE sr.registry_id=$1 AND sr.organization_id=$2 AND sr.source_system_id=$3 AND sr.source_record_key=$4), \
         (SELECT COUNT(*) FROM litecr.mutation_audit WHERE actor_id=$5 AND correlation_id=$6), \
         (SELECT COUNT(*) FROM litecr.outbox_event WHERE correlation_id=$6)",
    )
    .bind(registry)
    .bind(organization)
    .bind(source_system)
    .bind(source_record_key)
    .bind(actor_id)
    .bind(correlation_id)
    .fetch_one(pool)
    .await
    .map_err(|_| "could not compare effects around simulated lost-response retry".to_owned())
}

fn ensure(condition: bool, message: &str) -> Result<(), String> {
    if condition {
        Ok(())
    } else {
        Err(message.to_owned())
    }
}

fn debug_error(error: impl std::fmt::Debug) -> String {
    format!("synthetic registry operation failed: {error:?}")
}

async fn cleanup(database_url: &str, key: &str) {
    let pool = connect(database_url, 2)
        .await
        .expect("synthetic database available for cleanup");
    let actor_id = format!("synthetic-registrar-{key}");
    let registry = format!("registry-test-{key}");
    let organization = format!("organization-test-{key}");
    let mut tx = pool.begin().await.expect("cleanup transaction begins");
    sqlx::query(
        "DELETE FROM litecr.consumer_receipt r USING litecr.outbox_event o \
         WHERE r.event_id=o.event_id AND o.correlation_id=$1",
    )
    .bind(format!("correlation-{key}"))
    .execute(&mut *tx)
    .await
    .expect("only this scenario's correlated worker receipts are removed");
    sqlx::query(
        "DELETE FROM litecr.consumer_job j USING litecr.outbox_event o \
         WHERE j.event_id=o.event_id AND o.correlation_id=$1",
    )
    .bind(format!("correlation-{key}"))
    .execute(&mut *tx)
    .await
    .expect("only this scenario's correlated worker jobs are removed");
    sqlx::query("DELETE FROM litecr.outbox_event WHERE correlation_id = $1")
        .bind(format!("correlation-{key}"))
        .execute(&mut *tx)
        .await
        .expect("only this scenario's correlated outbox events are removed");
    sqlx::query("DELETE FROM litecr.mutation_audit WHERE request_id = $1")
        .bind(format!("request-{key}"))
        .execute(&mut *tx)
        .await
        .expect("only this scenario's audit events are removed");
    sqlx::query("DELETE FROM litecr.registry_idempotency WHERE actor_id = $1 AND registry_id = $2")
        .bind(&actor_id)
        .bind(&registry)
        .execute(&mut *tx)
        .await
        .expect("only this scenario's idempotency rows are removed");
    sqlx::query(
        "DELETE FROM litecr.person_identifier WHERE registry_id = $1 AND organization_id = $2",
    )
    .bind(&registry)
    .bind(&organization)
    .execute(&mut *tx)
    .await
    .expect("only this scenario's identifiers are removed");
    sqlx::query(
        "DELETE FROM litecr.source_identifier_assertion WHERE registry_id = $1 AND organization_id = $2",
    )
    .bind(&registry)
    .bind(&organization)
    .execute(&mut *tx)
    .await
    .expect("only this scenario's identifier assertions are removed");
    sqlx::query(
        "DELETE FROM matching.normalized_assertion WHERE source_record_id IN \
         (SELECT source_record_id FROM litecr.source_record WHERE registry_id = $1 AND organization_id = $2)",
    )
    .bind(&registry)
    .bind(&organization)
    .execute(&mut *tx)
    .await
    .expect("only this scenario's normalized assertions are removed");
    sqlx::query(
        "DELETE FROM matching.normalized_identifier WHERE registry_id = $1 AND organization_id = $2",
    )
    .bind(&registry)
    .bind(&organization)
    .execute(&mut *tx)
    .await
    .expect("only this scenario's normalized identifiers are removed");
    sqlx::query(
        "DELETE FROM matching.indexed_source_version WHERE source_record_id IN \
         (SELECT source_record_id FROM litecr.source_record WHERE registry_id = $1 AND organization_id = $2)",
    )
    .bind(&registry)
    .bind(&organization)
    .execute(&mut *tx)
    .await
    .expect("only this scenario's index markers are removed");
    sqlx::query(
        "DELETE FROM litecr.source_assertion WHERE source_record_id IN \
         (SELECT source_record_id FROM litecr.source_record WHERE registry_id = $1 AND organization_id = $2)",
    )
    .bind(&registry)
    .bind(&organization)
    .execute(&mut *tx)
    .await
    .expect("only this scenario's source assertions are removed");
    sqlx::query(
        "DELETE FROM litecr.source_record_version WHERE source_record_id IN \
         (SELECT source_record_id FROM litecr.source_record WHERE registry_id = $1 AND organization_id = $2)",
    )
    .bind(&registry)
    .bind(&organization)
    .execute(&mut *tx)
    .await
    .expect("only this scenario's source versions are removed");
    sqlx::query("DELETE FROM litecr.source_record WHERE registry_id = $1 AND organization_id = $2")
        .bind(&registry)
        .bind(&organization)
        .execute(&mut *tx)
        .await
        .expect("only this scenario's source records are removed");
    sqlx::query("DELETE FROM litecr.person_alias WHERE registry_id = $1 AND organization_id = $2")
        .bind(&registry)
        .bind(&organization)
        .execute(&mut *tx)
        .await
        .expect("only this scenario's aliases are removed");
    sqlx::query("DELETE FROM litecr.person WHERE registry_id = $1 AND organization_id = $2")
        .bind(&registry)
        .bind(&organization)
        .execute(&mut *tx)
        .await
        .expect("only this scenario's people are removed");
    tx.commit().await.expect("cleanup transaction commits");
}
