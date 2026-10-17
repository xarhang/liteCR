//! PostgreSQL registry adapter.

use crate::matching::index_accepted_source_version;
use async_trait::async_trait;
use litecr_application::{
    access::ActorKind,
    registry::{RegistryActor, RegistryStore, RegistryStoreError},
};
use litecr_domain::registry::{
    IdentifierView, IngestionOutcome, IngestionReceipt, PersonView, SourceAssertionView,
    SourceRecordCommand,
};
use serde_json::json;
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};

#[derive(Clone)]
pub struct PostgresRegistryStore {
    pool: PgPool,
}

impl PostgresRegistryStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

fn actor_kind_name(kind: ActorKind) -> &'static str {
    match kind {
        ActorKind::Human => "human",
        ActorKind::IntegrationClient => "integration_client",
        ActorKind::System => "system",
    }
}

fn put_hash_field(hasher: &mut Sha256, value: Option<&str>) {
    match value {
        Some(value) => {
            hasher.update([1]);
            hasher.update((value.len() as u64).to_be_bytes());
            hasher.update(value.as_bytes());
        }
        None => hasher.update([0]),
    }
}

fn command_hash(command: &SourceRecordCommand) -> [u8; 32] {
    let mut hasher = Sha256::new();
    put_hash_field(&mut hasher, Some("litecr-api-v1/source.upsert"));
    for value in [
        Some(command.registry_id.as_str()),
        Some(command.organization_id.as_str()),
        Some(command.source_system_id.as_str()),
        Some(command.source_record_key.as_str()),
        command.source_version.as_deref(),
    ] {
        put_hash_field(&mut hasher, value);
    }
    hasher.update((command.assertions.len() as u64).to_be_bytes());
    for assertion in &command.assertions {
        put_hash_field(&mut hasher, Some(&assertion.field_path));
        put_hash_field(&mut hasher, Some(&assertion.original_value));
        put_hash_field(&mut hasher, assertion.language_tag.as_deref());
        put_hash_field(&mut hasher, assertion.script.as_deref());
    }
    hasher.update((command.identifiers.len() as u64).to_be_bytes());
    for identifier in &command.identifiers {
        put_hash_field(&mut hasher, Some(&identifier.authority));
        put_hash_field(&mut hasher, Some(&identifier.identifier_type));
        put_hash_field(&mut hasher, Some(&identifier.value));
        hasher.update([u8::from(identifier.authoritative)]);
    }
    hasher.finalize().into()
}

fn digest(value: &str) -> [u8; 32] {
    Sha256::digest(value.as_bytes()).into()
}

fn parse_outcome(value: &str) -> Result<IngestionOutcome, RegistryStoreError> {
    match value {
        "accepted" => Ok(IngestionOutcome::Accepted),
        "source_conflict" => Ok(IngestionOutcome::SourceConflict),
        _ => Err(RegistryStoreError::Unavailable),
    }
}

#[async_trait]
impl RegistryStore for PostgresRegistryStore {
    async fn ingest(
        &self,
        actor: &RegistryActor,
        idempotency_key: &str,
        command: &SourceRecordCommand,
    ) -> Result<IngestionReceipt, RegistryStoreError> {
        let actor_kind = actor_kind_name(actor.kind);
        let issuer = actor.issuer_url.as_deref().unwrap_or_default();
        let fingerprint = command_hash(command);
        let key_hash = digest(idempotency_key);
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|_| RegistryStoreError::Unavailable)?;

        let prior = sqlx::query(
            "SELECT command_fingerprint, person_public_id, source_record_id::text AS source_record_id, \
             source_receive_version, outcome FROM litecr.registry_idempotency \
             WHERE actor_kind = $1 AND actor_issuer_url = $2 AND actor_id = $3 AND operation = 'source.upsert' \
             AND registry_id = $4 AND idempotency_key_hash = $5 FOR UPDATE",
        )
        .bind(actor_kind)
        .bind(issuer)
        .bind(&actor.id)
        .bind(&command.registry_id)
        .bind(key_hash.to_vec())
        .fetch_optional(&mut *tx)
        .await
        .map_err(|_| RegistryStoreError::Unavailable)?;
        if let Some(prior) = prior {
            let source_id: String = prior
                .try_get("source_record_id")
                .map_err(|_| RegistryStoreError::Unavailable)?;
            let withdrawn: bool = sqlx::query_scalar(
                "SELECT withdrawn_at IS NOT NULL FROM litecr.source_record WHERE source_record_id=$1::uuid",
            )
            .bind(&source_id)
            .fetch_one(&mut *tx)
            .await
            .map_err(|_| RegistryStoreError::Unavailable)?;
            if withdrawn {
                return Err(RegistryStoreError::Unavailable);
            }
            let prior_fingerprint: Vec<u8> = prior
                .try_get("command_fingerprint")
                .map_err(|_| RegistryStoreError::Unavailable)?;
            if prior_fingerprint.as_slice() != fingerprint {
                tx.rollback()
                    .await
                    .map_err(|_| RegistryStoreError::Unavailable)?;
                return Err(RegistryStoreError::IdempotencyKeyReused);
            }
            let receipt = receipt_from_row(&prior, true)?;
            tx.commit()
                .await
                .map_err(|_| RegistryStoreError::Unavailable)?;
            return Ok(receipt);
        }

        // Serialize first creation/update for this source key without holding a global lock.
        let lock_material = format!(
            "{}:{}:{}:{}:{}:{}:{}:{}",
            command.registry_id.len(),
            command.registry_id,
            command.organization_id.len(),
            command.organization_id,
            command.source_system_id.len(),
            command.source_system_id,
            command.source_record_key.len(),
            command.source_record_key
        );
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(lock_material)
            .execute(&mut *tx)
            .await
            .map_err(|_| RegistryStoreError::Unavailable)?;

        // A same-key request can have passed the first lookup before waiting on the source lock.
        // Recheck inside the serialized section so concurrent retries return the committed result.
        let prior = sqlx::query(
            "SELECT command_fingerprint, person_public_id, source_record_id::text AS source_record_id, \
             source_receive_version, outcome FROM litecr.registry_idempotency \
             WHERE actor_kind = $1 AND actor_issuer_url = $2 AND actor_id = $3 AND operation = 'source.upsert' \
             AND registry_id = $4 AND idempotency_key_hash = $5 FOR UPDATE",
        )
        .bind(actor_kind)
        .bind(issuer)
        .bind(&actor.id)
        .bind(&command.registry_id)
        .bind(key_hash.to_vec())
        .fetch_optional(&mut *tx)
        .await
        .map_err(|_| RegistryStoreError::Unavailable)?;
        if let Some(prior) = prior {
            let source_id: String = prior
                .try_get("source_record_id")
                .map_err(|_| RegistryStoreError::Unavailable)?;
            let withdrawn: bool = sqlx::query_scalar(
                "SELECT withdrawn_at IS NOT NULL FROM litecr.source_record WHERE source_record_id=$1::uuid",
            )
            .bind(&source_id)
            .fetch_one(&mut *tx)
            .await
            .map_err(|_| RegistryStoreError::Unavailable)?;
            if withdrawn {
                return Err(RegistryStoreError::Unavailable);
            }
            let prior_fingerprint: Vec<u8> = prior
                .try_get("command_fingerprint")
                .map_err(|_| RegistryStoreError::Unavailable)?;
            if prior_fingerprint.as_slice() != fingerprint {
                tx.rollback()
                    .await
                    .map_err(|_| RegistryStoreError::Unavailable)?;
                return Err(RegistryStoreError::IdempotencyKeyReused);
            }
            let receipt = receipt_from_row(&prior, true)?;
            tx.commit()
                .await
                .map_err(|_| RegistryStoreError::Unavailable)?;
            return Ok(receipt);
        }

        let current = sqlx::query(
            "SELECT sr.source_record_id::text AS source_record_id, sr.person_id::text AS person_id, \
             p.public_id, (sr.withdrawn_at IS NOT NULL) AS source_withdrawn, \
             sv.source_version, sv.payload_hash \
             FROM litecr.source_record sr JOIN litecr.person p ON p.person_id = sr.person_id \
             JOIN litecr.source_record_version sv ON sv.source_record_id = sr.source_record_id \
               AND sv.receive_version = sr.current_receive_version \
             WHERE sr.registry_id = $1 AND sr.organization_id = $2 AND sr.source_system_id = $3 \
               AND sr.source_record_key = $4 FOR UPDATE OF sr, p",
        )
        .bind(&command.registry_id)
        .bind(&command.organization_id)
        .bind(&command.source_system_id)
        .bind(&command.source_record_key)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|_| RegistryStoreError::Unavailable)?;

        let (source_id, person_id, public_id) = if let Some(row) = &current {
            if row
                .try_get::<bool, _>("source_withdrawn")
                .map_err(|_| RegistryStoreError::Unavailable)?
            {
                return Err(RegistryStoreError::Unavailable);
            }
            (
                row.try_get::<String, _>("source_record_id")
                    .map_err(|_| RegistryStoreError::Unavailable)?,
                row.try_get::<String, _>("person_id")
                    .map_err(|_| RegistryStoreError::Unavailable)?,
                row.try_get::<String, _>("public_id")
                    .map_err(|_| RegistryStoreError::Unavailable)?,
            )
        } else {
            let person = sqlx::query(
                "INSERT INTO litecr.person (public_id, registry_id, organization_id) \
                 VALUES ('per-' || gen_random_uuid()::text, $1, $2) \
                 RETURNING person_id::text AS person_id, public_id",
            )
            .bind(&command.registry_id)
            .bind(&command.organization_id)
            .fetch_one(&mut *tx)
            .await
            .map_err(|_| RegistryStoreError::Unavailable)?;
            let person_id: String = person
                .try_get("person_id")
                .map_err(|_| RegistryStoreError::Unavailable)?;
            let public_id: String = person
                .try_get("public_id")
                .map_err(|_| RegistryStoreError::Unavailable)?;
            sqlx::query(
                "INSERT INTO litecr.person_alias (registry_id, organization_id, alias_value, person_id) \
                 SELECT $1, $2, $3, person_id FROM litecr.person WHERE person_id::text = $4",
            )
            .bind(&command.registry_id)
            .bind(&command.organization_id)
            .bind(&public_id)
            .bind(&person_id)
            .execute(&mut *tx)
            .await
            .map_err(|_| RegistryStoreError::Unavailable)?;
            let row = sqlx::query(
                "INSERT INTO litecr.source_record (registry_id, organization_id, source_system_id, source_record_key, person_id) \
                 VALUES ($1, $2, $3, $4, $5::uuid) RETURNING source_record_id::text AS source_record_id",
            )
            .bind(&command.registry_id)
            .bind(&command.organization_id)
            .bind(&command.source_system_id)
            .bind(&command.source_record_key)
            .bind(&person_id)
            .fetch_one(&mut *tx)
            .await
            .map_err(|_| RegistryStoreError::Unavailable)?;
            let source_id: String = row
                .try_get("source_record_id")
                .map_err(|_| RegistryStoreError::Unavailable)?;
            (source_id, person_id, public_id)
        };

        let payload_hash = fingerprint;
        let receive_version = sqlx::query_scalar::<_, i64>(
            "SELECT COALESCE(MAX(receive_version), 0) + 1 FROM litecr.source_record_version WHERE source_record_id = $1::uuid",
        )
        .bind(&source_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(|_| RegistryStoreError::Unavailable)?;
        let exact_prior = sqlx::query(
            "SELECT receive_version, version_state FROM litecr.source_record_version \
             WHERE source_record_id = $1::uuid AND source_version IS NOT DISTINCT FROM $2 \
               AND payload_hash = $3 ORDER BY receive_version DESC LIMIT 1",
        )
        .bind(&source_id)
        .bind(&command.source_version)
        .bind(payload_hash.to_vec())
        .fetch_optional(&mut *tx)
        .await
        .map_err(|_| RegistryStoreError::Unavailable)?;
        if let Some(row) = exact_prior {
            let prior_version: i64 = row
                .try_get("receive_version")
                .map_err(|_| RegistryStoreError::Unavailable)?;
            let prior_state: String = row
                .try_get("version_state")
                .map_err(|_| RegistryStoreError::Unavailable)?;
            let receipt = IngestionReceipt {
                person_public_id: public_id,
                source_record_id: source_id.clone(),
                source_receive_version: prior_version,
                outcome: parse_outcome(&prior_state)?,
                replayed: true,
            };
            insert_idempotency(&mut tx, actor, command, &key_hash, &fingerprint, &receipt).await?;
            tx.commit()
                .await
                .map_err(|_| RegistryStoreError::Unavailable)?;
            return Ok(receipt);
        }
        let mut reason_code = None;
        if let Some(row) = &current {
            let prior_version: Option<String> = row
                .try_get("source_version")
                .map_err(|_| RegistryStoreError::Unavailable)?;
            let prior_hash: Vec<u8> = row
                .try_get("payload_hash")
                .map_err(|_| RegistryStoreError::Unavailable)?;
            if prior_version == command.source_version && prior_hash.as_slice() != payload_hash {
                reason_code = Some("SOURCE_VERSION_CONFLICT");
            }
        }

        // Hold the current identity transactionally; verified authority collisions remain assertions,
        // but never replace an already accepted identifier.
        let mut identifier_conflict = false;
        let mut ordered_identifiers = command.identifiers.iter().collect::<Vec<_>>();
        ordered_identifiers.sort_by(|left, right| {
            (&left.authority, &left.identifier_type, &left.value).cmp(&(
                &right.authority,
                &right.identifier_type,
                &right.value,
            ))
        });
        for identifier in ordered_identifiers {
            if identifier.authoritative {
                let identifier_lock = format!(
                    "{}:{}:{}:{}:{}:{}:{}:{}",
                    command.registry_id.len(),
                    command.registry_id,
                    identifier.authority.len(),
                    identifier.authority,
                    identifier.identifier_type.len(),
                    identifier.identifier_type,
                    identifier.value.len(),
                    identifier.value
                );
                sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
                    .bind(identifier_lock)
                    .execute(&mut *tx)
                    .await
                    .map_err(|_| RegistryStoreError::Unavailable)?;
                let owner = sqlx::query_scalar::<_, String>(
                    "SELECT person_id::text FROM litecr.person_identifier \
                     WHERE registry_id = $1 AND authority = $2 \
                       AND identifier_type = $3 AND identifier_value = $4 AND person_id <> $5::uuid \
                     FOR UPDATE",
                )
                .bind(&command.registry_id)
                .bind(&identifier.authority)
                .bind(&identifier.identifier_type)
                .bind(&identifier.value)
                .bind(&person_id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(|_| RegistryStoreError::Unavailable)?;
                if owner.is_some() {
                    identifier_conflict = true;
                    reason_code = Some("AUTHORITATIVE_IDENTIFIER_CONFLICT");
                }
            }
        }
        let outcome = if reason_code.is_some() || identifier_conflict {
            IngestionOutcome::SourceConflict
        } else {
            IngestionOutcome::Accepted
        };
        let outcome_name = match outcome {
            IngestionOutcome::Accepted => "accepted",
            IngestionOutcome::SourceConflict => "source_conflict",
        };
        sqlx::query(
            "INSERT INTO litecr.source_record_version (source_record_id, receive_version, source_version, payload_hash, version_state) \
             VALUES ($1::uuid, $2, $3, $4, $5)",
        )
        .bind(&source_id)
        .bind(receive_version)
        .bind(&command.source_version)
        .bind(payload_hash.to_vec())
        .bind(outcome_name)
        .execute(&mut *tx)
        .await
        .map_err(|_| RegistryStoreError::Unavailable)?;
        for assertion in &command.assertions {
            sqlx::query(
                "INSERT INTO litecr.source_assertion (source_record_id, receive_version, field_path, original_value, language_tag, script) \
                 VALUES ($1::uuid, $2, $3, $4, $5, $6)",
            )
            .bind(&source_id)
            .bind(receive_version)
            .bind(&assertion.field_path)
            .bind(&assertion.original_value)
            .bind(&assertion.language_tag)
            .bind(&assertion.script)
            .execute(&mut *tx)
            .await
            .map_err(|_| RegistryStoreError::Unavailable)?;
        }
        for identifier in &command.identifiers {
            let conflict = identifier.authoritative
                && sqlx::query_scalar::<_, bool>(
                    "SELECT EXISTS (SELECT 1 FROM litecr.person_identifier WHERE registry_id = $1 \
                     AND authority = $2 AND identifier_type = $3 AND identifier_value = $4 \
                     AND person_id <> $5::uuid)",
                )
                .bind(&command.registry_id)
                .bind(&identifier.authority)
                .bind(&identifier.identifier_type)
                .bind(&identifier.value)
                .bind(&person_id)
                .fetch_one(&mut *tx)
                .await
                .map_err(|_| RegistryStoreError::Unavailable)?;
            let assertion_state = if conflict { "conflict" } else { "accepted" };
            sqlx::query(
                "INSERT INTO litecr.source_identifier_assertion \
                 (source_record_id, receive_version, registry_id, organization_id, authority, identifier_type, original_value, authoritative, assertion_state) \
                 VALUES ($1::uuid, $2, $3, $4, $5, $6, $7, $8, $9)",
            )
            .bind(&source_id)
            .bind(receive_version)
            .bind(&command.registry_id)
            .bind(&command.organization_id)
            .bind(&identifier.authority)
            .bind(&identifier.identifier_type)
            .bind(&identifier.value)
            .bind(identifier.authoritative)
            .bind(assertion_state)
            .execute(&mut *tx)
            .await
            .map_err(|_| RegistryStoreError::Unavailable)?;
            if identifier.authoritative && !conflict {
                sqlx::query(
                    "INSERT INTO litecr.person_identifier \
                     (registry_id, organization_id, authority, identifier_type, identifier_value, person_id, source_record_id, authoritative) \
                     VALUES ($1, $2, $3, $4, $5, $6::uuid, $7::uuid, TRUE) ON CONFLICT DO NOTHING",
                )
                .bind(&command.registry_id)
                .bind(&command.organization_id)
                .bind(&identifier.authority)
                .bind(&identifier.identifier_type)
                .bind(&identifier.value)
                .bind(&person_id)
                .bind(&source_id)
                .execute(&mut *tx)
                .await
                .map_err(|_| RegistryStoreError::Unavailable)?;
            }
        }
        if outcome == IngestionOutcome::Accepted {
            index_accepted_source_version(
                &mut tx,
                &source_id,
                &command.registry_id,
                &command.organization_id,
                receive_version,
                &command.assertions,
                &command.identifiers,
            )
            .await
            .map_err(|_| RegistryStoreError::Unavailable)?;
            sqlx::query(
                "UPDATE litecr.source_record SET current_receive_version = $2, updated_at = now() WHERE source_record_id = $1::uuid",
            )
            .bind(&source_id)
            .bind(receive_version)
            .execute(&mut *tx)
            .await
            .map_err(|_| RegistryStoreError::Unavailable)?;
            if current.is_some() {
                sqlx::query("UPDATE litecr.person SET resource_version = resource_version + 1 WHERE person_id = $1::uuid")
                    .bind(&person_id)
                    .execute(&mut *tx)
                    .await
                    .map_err(|_| RegistryStoreError::Unavailable)?;
            }
        }

        let receipt = IngestionReceipt {
            person_public_id: public_id,
            source_record_id: source_id.clone(),
            source_receive_version: receive_version,
            outcome,
            replayed: false,
        };
        let audit_action = if outcome == IngestionOutcome::Accepted {
            "registry.source_record.accept"
        } else {
            "registry.source_record.conflict"
        };
        sqlx::query(
            "INSERT INTO litecr.mutation_audit \
             (actor_kind, actor_issuer_url, actor_id, registry_id, organization_id, action, resource_type, resource_ref, outcome, reason_code, request_id, correlation_id, resource_version) \
             VALUES ($1, $2, $3, $4, $5, $6, 'source_record', $7, $8, $9, $10, $11, $12)",
        )
        .bind(actor_kind)
        .bind(issuer)
        .bind(&actor.id)
        .bind(&command.registry_id)
        .bind(&command.organization_id)
        .bind(audit_action)
        .bind(&source_id)
        .bind(outcome_name)
        .bind(reason_code)
        .bind(&actor.request_id)
        .bind(&actor.correlation_id)
        .bind(receive_version)
        .execute(&mut *tx)
        .await
        .map_err(|_| RegistryStoreError::Unavailable)?;

        let event_type = if outcome == IngestionOutcome::Accepted {
            "registry.source_record.accepted"
        } else {
            "registry.source_record.conflict"
        };
        let event_payload = json!({
            "source_system_id": command.source_system_id,
            "source_record_id": source_id,
            "source_receive_version": receive_version,
            "person_public_id": receipt.person_public_id,
            "outcome": outcome_name,
            "reason_code": reason_code,
        });
        sqlx::query(
            "INSERT INTO litecr.outbox_event \
             (event_type, schema_version, registry_id, organization_id, aggregate_type, aggregate_id, aggregate_version, actor_kind, actor_id, correlation_id, payload) \
             VALUES ($1, 1, $2, $3, 'source_record', $4::uuid, $5, $6, $7, $8, $9)",
        )
        .bind(event_type)
        .bind(&command.registry_id)
        .bind(&command.organization_id)
        .bind(&source_id)
        .bind(receive_version)
        .bind(actor_kind)
        .bind(&actor.id)
        .bind(&actor.correlation_id)
        .bind(event_payload)
        .execute(&mut *tx)
        .await
        .map_err(|_| RegistryStoreError::Unavailable)?;
        insert_idempotency(&mut tx, actor, command, &key_hash, &fingerprint, &receipt).await?;
        tx.commit()
            .await
            .map_err(|_| RegistryStoreError::Unavailable)?;
        Ok(receipt)
    }

    async fn person(
        &self,
        registry_id: &str,
        organization_id: &str,
        public_id_or_alias: &str,
    ) -> Result<Option<PersonView>, RegistryStoreError> {
        let person = sqlx::query(
            "WITH RECURSIVE person_chain(person_id,depth,path) AS ( \
               SELECT a.person_id,0,ARRAY[a.person_id] FROM litecr.person_alias a \
               WHERE a.registry_id=$1 AND a.organization_id=$2 AND a.alias_value=$3 \
               UNION ALL \
               SELECT red.target_person_id,pc.depth+1,pc.path || red.target_person_id \
               FROM person_chain pc JOIN litecr.person_redirect red \
                 ON red.registry_id=$1 AND red.organization_id=$2 AND red.source_person_id=pc.person_id AND red.active=TRUE \
               WHERE NOT red.target_person_id=ANY(pc.path) \
             ) SELECT p.person_id::text AS person_id,p.public_id,p.resource_version \
               FROM person_chain pc JOIN LATERAL ( \
                 SELECT person_id,public_id,resource_version,registry_id,organization_id \
                 FROM litecr.person WHERE person_id=pc.person_id OFFSET 0 \
               ) p ON TRUE \
              WHERE p.registry_id=$1 AND p.organization_id=$2 AND EXISTS ( \
                SELECT 1 FROM litecr.source_record sr \
                JOIN litecr.source_record_version sv ON sv.source_record_id=sr.source_record_id AND sv.receive_version=sr.current_receive_version \
                WHERE sr.registry_id=$1 AND sr.organization_id=$2 AND sr.person_id=p.person_id \
                  AND sr.withdrawn_at IS NULL AND sv.withdrawn_at IS NULL AND sv.version_state='accepted' \
              ) ORDER BY pc.depth DESC LIMIT 1",
        )
        .bind(registry_id)
        .bind(organization_id)
        .bind(public_id_or_alias)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| RegistryStoreError::Unavailable)?;
        let Some(person) = person else {
            return Ok(None);
        };
        let person_id: String = person
            .try_get("person_id")
            .map_err(|_| RegistryStoreError::Unavailable)?;
        let public_id: String = person
            .try_get("public_id")
            .map_err(|_| RegistryStoreError::Unavailable)?;
        let resource_version: i64 = person
            .try_get("resource_version")
            .map_err(|_| RegistryStoreError::Unavailable)?;
        // Resolve each incoming graph UUID before applying alias scope. Keeping
        // scope outside OFFSET 0 prevents a cached low-cardinality plan from
        // choosing a whole-organization prefix scan instead of the person key.
        let aliases = sqlx::query_scalar::<_, String>(
            "WITH RECURSIVE alias_people(person_id,path) AS ( \
               SELECT $3::uuid,ARRAY[$3::uuid] \
               UNION ALL \
               SELECT red.source_person_id,ap.path || red.source_person_id \
               FROM alias_people ap JOIN litecr.person_redirect red \
                 ON red.registry_id=$1 AND red.organization_id=$2 AND red.target_person_id=ap.person_id AND red.active=TRUE \
               WHERE NOT red.source_person_id=ANY(ap.path) \
             ) SELECT DISTINCT a.alias_value FROM alias_people ap JOIN LATERAL ( \
               SELECT pa.alias_value,pa.registry_id,pa.organization_id FROM litecr.person_alias pa \
               WHERE pa.person_id=ap.person_id OFFSET 0 \
             ) a ON a.registry_id=$1 AND a.organization_id=$2 ORDER BY a.alias_value",
        )
        .bind(registry_id)
        .bind(organization_id)
        .bind(&person_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|_| RegistryStoreError::Unavailable)?;
        // Bound point lookups by UUID before applying scope checks. Missing
        // statistics and generic TEXT -> UUID plans otherwise favor scoped-prefix
        // scans. OFFSET 0 preserves the lookup boundary; materialization prevents
        // repeating the resulting source/assertion projection.
        let identifiers = sqlx::query(
            "WITH person_sources AS MATERIALIZED ( \
               SELECT sr.source_record_id,sr.current_receive_version FROM ( \
                 SELECT source_record_id,current_receive_version,registry_id,organization_id,withdrawn_at \
                 FROM litecr.source_record WHERE person_id=$3::uuid OFFSET 0 \
               ) sr WHERE sr.registry_id=$1 AND sr.organization_id=$2 AND sr.withdrawn_at IS NULL \
             ), accepted_identifiers AS MATERIALIZED ( \
               SELECT sia.authority,sia.identifier_type,sia.original_value,sia.authoritative, \
                      sia.receive_version,ps.current_receive_version,sv.version_state \
               FROM person_sources ps JOIN LATERAL ( \
                 SELECT source_record_id,receive_version,registry_id,organization_id,authority,identifier_type,original_value,authoritative,assertion_state \
                 FROM litecr.source_identifier_assertion WHERE source_record_id=ps.source_record_id AND withdrawn_at IS NULL OFFSET 0 \
               ) sia ON TRUE \
               JOIN litecr.source_record_version sv ON sv.source_record_id=sia.source_record_id AND sv.receive_version=sia.receive_version \
               WHERE sia.registry_id=$1 AND sia.organization_id=$2 AND sia.assertion_state='accepted' \
                 AND sv.withdrawn_at IS NULL \
             ) SELECT authority,identifier_type,identifier_value,bool_or(authoritative) AS authoritative FROM ( \
               SELECT pi.authority,pi.identifier_type,pi.identifier_value,pi.authoritative \
               FROM litecr.person_identifier pi WHERE pi.registry_id=$1 AND pi.organization_id=$2 AND pi.person_id=$3::uuid \
                 AND EXISTS (SELECT 1 FROM accepted_identifiers ai WHERE ai.authority=pi.authority \
                   AND ai.identifier_type=pi.identifier_type AND ai.original_value=pi.identifier_value) \
               UNION ALL \
               SELECT authority,identifier_type,original_value AS identifier_value,FALSE AS authoritative \
               FROM accepted_identifiers WHERE authoritative=FALSE AND version_state='accepted' AND receive_version=current_receive_version \
             ) visible_identifiers GROUP BY authority,identifier_type,identifier_value ORDER BY authority,identifier_type,identifier_value",
        )
        .bind(registry_id)
        .bind(organization_id)
        .bind(&person_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|_| RegistryStoreError::Unavailable)?
        .iter()
        .map(|row| {
            Ok(IdentifierView {
                authority: row
                    .try_get("authority")
                    .map_err(|_| RegistryStoreError::Unavailable)?,
                identifier_type: row
                    .try_get("identifier_type")
                    .map_err(|_| RegistryStoreError::Unavailable)?,
                value: row
                    .try_get("identifier_value")
                    .map_err(|_| RegistryStoreError::Unavailable)?,
                authoritative: row
                    .try_get("authoritative")
                    .map_err(|_| RegistryStoreError::Unavailable)?,
            })
        })
        .collect::<Result<Vec<_>, RegistryStoreError>>()?;
        let records = sqlx::query(
            "SELECT sr.source_record_id::text AS source_record_id,sr.source_system_id,sr.current_receive_version \
             FROM (SELECT source_record_id,source_system_id,current_receive_version,registry_id,organization_id,withdrawn_at \
               FROM litecr.source_record WHERE person_id=$3::uuid OFFSET 0) sr \
             WHERE sr.registry_id=$1 AND sr.organization_id=$2 AND sr.withdrawn_at IS NULL ORDER BY source_record_id",
        )
        .bind(registry_id)
        .bind(organization_id)
        .bind(&person_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|_| RegistryStoreError::Unavailable)?;
        let mut source_record_ids = Vec::new();
        let mut assertions = Vec::new();
        for record in records {
            let source_record_id: String = record
                .try_get("source_record_id")
                .map_err(|_| RegistryStoreError::Unavailable)?;
            let source_system_id: String = record
                .try_get("source_system_id")
                .map_err(|_| RegistryStoreError::Unavailable)?;
            source_record_ids.push(source_record_id.clone());
            let rows = sqlx::query(
                "SELECT a.receive_version, a.field_path, a.original_value, a.language_tag, a.script, sv.source_version \
                 FROM litecr.source_assertion a JOIN litecr.source_record_version sv \
                   ON sv.source_record_id = a.source_record_id AND sv.receive_version = a.receive_version \
                 WHERE a.source_record_id = $1::uuid AND a.withdrawn_at IS NULL AND sv.withdrawn_at IS NULL ORDER BY a.receive_version, a.field_path",
            )
            .bind(&source_record_id)
            .fetch_all(&self.pool)
            .await
            .map_err(|_| RegistryStoreError::Unavailable)?;
            for row in rows {
                assertions.push(SourceAssertionView {
                    source_record_id: source_record_id.clone(),
                    source_system_id: source_system_id.clone(),
                    source_version: row
                        .try_get("source_version")
                        .map_err(|_| RegistryStoreError::Unavailable)?,
                    receive_version: row
                        .try_get("receive_version")
                        .map_err(|_| RegistryStoreError::Unavailable)?,
                    field_path: row
                        .try_get("field_path")
                        .map_err(|_| RegistryStoreError::Unavailable)?,
                    original_value: row
                        .try_get("original_value")
                        .map_err(|_| RegistryStoreError::Unavailable)?,
                    language_tag: row
                        .try_get("language_tag")
                        .map_err(|_| RegistryStoreError::Unavailable)?,
                    script: row
                        .try_get("script")
                        .map_err(|_| RegistryStoreError::Unavailable)?,
                });
            }
        }
        Ok(Some(PersonView {
            public_id,
            resource_version,
            source_record_ids,
            assertions,
            aliases,
            identifiers,
        }))
    }

    async fn person_for_source(
        &self,
        registry_id: &str,
        organization_id: &str,
        source_record_id: &str,
    ) -> Result<Option<PersonView>, RegistryStoreError> {
        let public_id = sqlx::query_scalar::<_, String>(
            "SELECT p.public_id FROM litecr.source_record sr JOIN litecr.person p ON p.person_id=sr.person_id WHERE sr.source_record_id=$1::uuid AND sr.registry_id=$2 AND sr.organization_id=$3 AND sr.withdrawn_at IS NULL AND p.registry_id=$2 AND p.organization_id=$3",
        )
        .bind(source_record_id)
        .bind(registry_id)
        .bind(organization_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| RegistryStoreError::Unavailable)?;
        match public_id {
            Some(public_id) => self.person(registry_id, organization_id, &public_id).await,
            None => Ok(None),
        }
    }

    async fn search_public_id(
        &self,
        registry_id: &str,
        organization_id: &str,
        public_id_or_alias: &str,
        _limit: u16,
    ) -> Result<Vec<PersonView>, RegistryStoreError> {
        Ok(self
            .person(registry_id, organization_id, public_id_or_alias)
            .await?
            .into_iter()
            .collect())
    }

    async fn search_identifier(
        &self,
        registry_id: &str,
        organization_id: &str,
        authority: &str,
        value: &str,
        limit: u16,
    ) -> Result<Vec<PersonView>, RegistryStoreError> {
        self.search_identifier_in_scope(registry_id, organization_id, Some(authority), value, limit)
            .await
    }

    async fn search_identifier_value(
        &self,
        registry_id: &str,
        organization_id: &str,
        value: &str,
        limit: u16,
    ) -> Result<Vec<PersonView>, RegistryStoreError> {
        self.search_identifier_in_scope(registry_id, organization_id, None, value, limit)
            .await
    }
}

impl PostgresRegistryStore {
    async fn search_identifier_in_scope(
        &self,
        registry_id: &str,
        organization_id: &str,
        authority: Option<&str>,
        value: &str,
        limit: u16,
    ) -> Result<Vec<PersonView>, RegistryStoreError> {
        let public_ids = sqlx::query_scalar::<_, String>(
            "WITH RECURSIVE person_chain(person_id,depth,path) AS ( \
               SELECT p.person_id,0,ARRAY[p.person_id] FROM ( \
               SELECT i.person_id FROM litecr.person_identifier i \
               WHERE i.registry_id=$1 AND i.organization_id=$2 AND ($3::text IS NULL OR i.authority=$3) AND i.identifier_value=$4 \
                 AND EXISTS (SELECT 1 FROM litecr.source_identifier_assertion sia \
                   JOIN litecr.source_record sr ON sr.source_record_id=sia.source_record_id \
                   JOIN litecr.source_record_version sv ON sv.source_record_id=sia.source_record_id AND sv.receive_version=sia.receive_version \
                   WHERE sr.person_id=i.person_id AND sr.withdrawn_at IS NULL AND sv.withdrawn_at IS NULL AND sia.withdrawn_at IS NULL \
                     AND sia.registry_id=i.registry_id AND sia.authority=i.authority AND sia.identifier_type=i.identifier_type \
                     AND md5(sia.original_value)=md5($4) AND sia.original_value=i.identifier_value AND sia.assertion_state='accepted') \
               UNION \
               SELECT sr.person_id FROM litecr.source_identifier_assertion sia \
               JOIN litecr.source_record sr ON sr.source_record_id=sia.source_record_id \
               JOIN litecr.source_record_version sv ON sv.source_record_id=sia.source_record_id AND sv.receive_version=sia.receive_version \
               WHERE sia.registry_id=$1 AND sia.organization_id=$2 AND sr.registry_id=$1 AND sr.organization_id=$2 \
                 AND ($3::text IS NULL OR sia.authority=$3) AND md5(sia.original_value)=md5($4) AND sia.original_value=$4 \
                 AND sia.authoritative=FALSE AND sia.assertion_state='accepted' AND sv.version_state='accepted' \
                 AND sr.current_receive_version=sia.receive_version AND sr.withdrawn_at IS NULL AND sv.withdrawn_at IS NULL AND sia.withdrawn_at IS NULL \
               ) identifiers JOIN litecr.person p ON p.person_id=identifiers.person_id \
               WHERE p.registry_id=$1 AND p.organization_id=$2 \
               UNION ALL \
               SELECT red.target_person_id,pc.depth+1,pc.path || red.target_person_id \
               FROM person_chain pc JOIN litecr.person_redirect red \
                 ON red.registry_id=$1 AND red.organization_id=$2 AND red.source_person_id=pc.person_id AND red.active=TRUE \
               WHERE NOT red.target_person_id=ANY(pc.path) \
             ) SELECT DISTINCT p.public_id FROM person_chain pc \
               JOIN litecr.person p ON p.person_id=pc.person_id \
              WHERE p.registry_id=$1 AND p.organization_id=$2 ORDER BY p.public_id LIMIT $5",
        )
        .bind(registry_id)
        .bind(organization_id)
        .bind(authority)
        .bind(value)
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await
        .map_err(|_| RegistryStoreError::Unavailable)?;
        let mut people = Vec::with_capacity(public_ids.len());
        for public_id in public_ids {
            if let Some(person) = self
                .person(registry_id, organization_id, &public_id)
                .await?
            {
                people.push(person);
            }
        }
        Ok(people)
    }
}

async fn insert_idempotency(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    actor: &RegistryActor,
    command: &SourceRecordCommand,
    key_hash: &[u8; 32],
    fingerprint: &[u8; 32],
    receipt: &IngestionReceipt,
) -> Result<(), RegistryStoreError> {
    sqlx::query(
        "INSERT INTO litecr.registry_idempotency \
         (actor_kind, actor_issuer_url, actor_id, operation, registry_id, idempotency_key_hash, command_fingerprint, person_public_id, source_record_id, source_receive_version, outcome) \
         VALUES ($1, $2, $3, 'source.upsert', $4, $5, $6, $7, $8::uuid, $9, $10)",
    )
    .bind(actor_kind_name(actor.kind))
    .bind(actor.issuer_url.as_deref().unwrap_or_default())
    .bind(&actor.id)
    .bind(&command.registry_id)
    .bind(key_hash.to_vec())
    .bind(fingerprint.to_vec())
    .bind(&receipt.person_public_id)
    .bind(&receipt.source_record_id)
    .bind(receipt.source_receive_version)
    .bind(match receipt.outcome { IngestionOutcome::Accepted => "accepted", IngestionOutcome::SourceConflict => "source_conflict" })
    .execute(&mut **tx)
    .await
    .map_err(|_| RegistryStoreError::Unavailable)?;
    Ok(())
}

fn receipt_from_row(
    row: &sqlx::postgres::PgRow,
    replayed: bool,
) -> Result<IngestionReceipt, RegistryStoreError> {
    let outcome: String = row
        .try_get("outcome")
        .map_err(|_| RegistryStoreError::Unavailable)?;
    Ok(IngestionReceipt {
        person_public_id: row
            .try_get("person_public_id")
            .map_err(|_| RegistryStoreError::Unavailable)?,
        source_record_id: row
            .try_get("source_record_id")
            .map_err(|_| RegistryStoreError::Unavailable)?,
        source_receive_version: row
            .try_get("source_receive_version")
            .map_err(|_| RegistryStoreError::Unavailable)?,
        outcome: parse_outcome(&outcome)?,
        replayed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use litecr_domain::registry::{IdentifierAssertion, SourceAssertion};

    #[test]
    fn command_fingerprint_binds_script_partial_values_authority_and_scope() {
        let base = SourceRecordCommand {
            registry_id: "reg-synthetic".into(),
            organization_id: "org-synthetic".into(),
            source_system_id: "src-synthetic".into(),
            source_record_key: "key-synthetic".into(),
            source_version: Some("opaque-1".into()),
            assertions: vec![
                SourceAssertion {
                    field_path: "name.original".into(),
                    original_value: "ນາງ ບຸນມີ".into(),
                    language_tag: Some("lo".into()),
                    script: Some("Lao".into()),
                },
                SourceAssertion {
                    field_path: "birth_date".into(),
                    original_value: "1986-04".into(),
                    language_tag: None,
                    script: None,
                },
            ],
            identifiers: vec![IdentifierAssertion {
                authority: "urn:synthetic:district".into(),
                identifier_type: "synthetic".into(),
                value: "0007".into(),
                authoritative: true,
            }],
        };
        let baseline = command_hash(&base);
        assert_ne!(
            baseline,
            command_hash(&SourceRecordCommand {
                organization_id: "other-org".into(),
                ..base.clone()
            })
        );
        assert_ne!(
            baseline,
            command_hash(&SourceRecordCommand {
                assertions: vec![
                    SourceAssertion {
                        original_value: "1986-04-01".into(),
                        ..base.assertions[1].clone()
                    },
                    base.assertions[0].clone()
                ],
                ..base.clone()
            })
        );
        assert_ne!(
            baseline,
            command_hash(&SourceRecordCommand {
                assertions: vec![
                    SourceAssertion {
                        original_value: "นาง บุญมี".into(),
                        ..base.assertions[0].clone()
                    },
                    base.assertions[1].clone()
                ],
                ..base
            })
        );
    }

    #[test]
    fn idempotency_secret_is_only_stored_as_digest() {
        let key = "synthetic-high-entropy-key";
        assert_ne!(digest(key).as_slice(), key.as_bytes());
        assert_eq!(digest(key).len(), 32);
    }
}
