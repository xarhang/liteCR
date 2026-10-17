use async_trait::async_trait;
use litecr_application::localization::{
    CatalogEntry, CatalogError, CatalogRepository, DraftTranslation, Locale, MessageSource,
    PublishedBundle, ReviewDecision, TranslationState, valid_plural_forms, valid_plural_values,
};
use serde_json::{Value, json};
use sqlx::{PgPool, Row};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone)]
pub struct PostgresCatalogRepository {
    pool: PgPool,
}
impl PostgresCatalogRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}
fn map_db(error: sqlx::Error) -> CatalogError {
    if matches!(error, sqlx::Error::RowNotFound) {
        CatalogError::NotFound
    } else if matches!(error, sqlx::Error::Database(ref e) if e.code().as_deref()==Some("23505") || e.code().as_deref()==Some("40001"))
    {
        CatalogError::Conflict
    } else {
        CatalogError::Unavailable
    }
}
fn state(value: &str) -> Result<TranslationState, CatalogError> {
    match value {
        "draft" => Ok(TranslationState::Draft),
        "in_review" => Ok(TranslationState::InReview),
        "approved" => Ok(TranslationState::Approved),
        _ => Err(CatalogError::Unavailable),
    }
}
fn scope_bind(scope: &litecr_application::access::ResourceScope) -> (&str, &str) {
    (&scope.registry_id, &scope.organization_id)
}
fn placeholders(text: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut rest = text;
    while let Some(i) = rest.find("{{") {
        rest = &rest[i + 2..];
        let Some(j) = rest.find("}}") else {
            return BTreeSet::new();
        };
        out.insert(rest[..j].to_owned());
        rest = &rest[j + 2..];
    }
    out
}
fn locale(row: &sqlx::postgres::PgRow) -> Result<Locale, CatalogError> {
    Ok(Locale {
        tag: row
            .try_get("locale_tag")
            .map_err(|_| CatalogError::Unavailable)?,
        direction: row
            .try_get("text_direction")
            .map_err(|_| CatalogError::Unavailable)?,
        enabled: row
            .try_get("enabled")
            .map_err(|_| CatalogError::Unavailable)?,
    })
}
fn draft(row: &sqlx::postgres::PgRow) -> Result<DraftTranslation, CatalogError> {
    let text: String = row
        .try_get("translation_text")
        .map_err(|_| CatalogError::Unavailable)?;
    let source_revision: i64 = row
        .try_get("source_revision")
        .map_err(|_| CatalogError::Unavailable)?;
    let current: i64 = row
        .try_get("current_source_revision")
        .map_err(|_| CatalogError::Unavailable)?;
    let plural_forms: Option<Value> = row
        .try_get("plural_forms")
        .map_err(|_| CatalogError::Unavailable)?;
    Ok(DraftTranslation {
        locale: row
            .try_get("locale_tag")
            .map_err(|_| CatalogError::Unavailable)?,
        namespace: row
            .try_get("namespace")
            .map_err(|_| CatalogError::Unavailable)?,
        key: row
            .try_get("message_key")
            .map_err(|_| CatalogError::Unavailable)?,
        value: text,
        plural_forms: plural_forms
            .map(serde_json::from_value)
            .transpose()
            .map_err(|_| CatalogError::Unavailable)?,
        state: state(
            &row.try_get::<String, _>("state")
                .map_err(|_| CatalogError::Unavailable)?,
        )?,
        stale: source_revision != current,
        source_revision,
    })
}

#[async_trait]
impl CatalogRepository for PostgresCatalogRepository {
    async fn add_locale(
        &self,
        s: &litecr_application::access::ResourceScope,
        l: &Locale,
        a: &str,
    ) -> Result<(), CatalogError> {
        let (r, o) = scope_bind(s);
        let mut tx = self.pool.begin().await.map_err(map_db)?;
        // Serialize equivalent-tag registration without rewriting existing keys.
        let lock_key = format!(
            "litecr.catalog_locale:{r}:{o}:{}",
            l.tag.to_ascii_lowercase()
        );
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(lock_key)
            .execute(&mut *tx)
            .await
            .map_err(map_db)?;
        let matches = sqlx::query_scalar::<_, String>("SELECT locale_tag FROM litecr.catalog_locale WHERE registry_id=$1 AND organization_id=$2 AND lower(locale_tag)=lower($3) ORDER BY locale_tag LIMIT 2 FOR UPDATE")
            .bind(r).bind(o).bind(&l.tag).fetch_all(&mut *tx).await.map_err(map_db)?;
        if matches.len() > 1 {
            return Err(CatalogError::Conflict);
        }
        let storage_tag = matches.first().unwrap_or(&l.tag);
        sqlx::query("INSERT INTO litecr.catalog_locale(registry_id,organization_id,locale_tag,text_direction) VALUES($1,$2,$3,$4) ON CONFLICT(registry_id,organization_id,locale_tag) DO UPDATE SET text_direction=EXCLUDED.text_direction,enabled=TRUE").bind(r).bind(o).bind(storage_tag).bind(&l.direction).execute(&mut *tx).await.map_err(map_db)?;
        audit(&mut tx, r, o, a, "locale.add", storage_tag, None).await?;
        tx.commit().await.map_err(map_db)
    }
    async fn resolve_locale_tag(
        &self,
        s: &litecr_application::access::ResourceScope,
        canonical_tag: &str,
    ) -> Result<Option<String>, CatalogError> {
        let (r, o) = scope_bind(s);
        let matches = sqlx::query_scalar::<_, String>("SELECT locale_tag FROM litecr.catalog_locale WHERE registry_id=$1 AND organization_id=$2 AND lower(locale_tag)=lower($3) ORDER BY locale_tag LIMIT 2")
            .bind(r).bind(o).bind(canonical_tag).fetch_all(&self.pool).await.map_err(map_db)?;
        match matches.as_slice() {
            [] => Ok(None),
            [tag] => Ok(Some(tag.clone())),
            _ => Err(CatalogError::Conflict),
        }
    }
    async fn list_locales(
        &self,
        s: &litecr_application::access::ResourceScope,
    ) -> Result<Vec<Locale>, CatalogError> {
        let (r, o) = scope_bind(s);
        let rows=sqlx::query("SELECT locale_tag,text_direction,enabled FROM litecr.catalog_locale WHERE registry_id=$1 AND organization_id=$2 ORDER BY locale_tag").bind(r).bind(o).fetch_all(&self.pool).await.map_err(map_db)?;
        rows.iter().map(locale).collect()
    }
    async fn list_published_ui_locales(
        &self,
        s: &litecr_application::access::ResourceScope,
    ) -> Result<Vec<Locale>, CatalogError> {
        let (r, o) = scope_bind(s);
        let rows = sqlx::query("SELECT l.locale_tag,l.text_direction,l.enabled FROM litecr.catalog_locale l JOIN litecr.catalog_bundle_active a ON a.registry_id=l.registry_id AND a.organization_id=l.organization_id AND a.locale_tag=l.locale_tag AND a.namespace='ui' WHERE l.registry_id=$1 AND l.organization_id=$2 AND l.enabled=TRUE ORDER BY l.locale_tag")
            .bind(r).bind(o).fetch_all(&self.pool).await.map_err(map_db)?;
        rows.iter().map(locale).collect()
    }
    async fn set_source(
        &self,
        s: &litecr_application::access::ResourceScope,
        x: &MessageSource,
        a: &str,
    ) -> Result<(), CatalogError> {
        let (r, o) = scope_bind(s);
        let mut tx = self.pool.begin().await.map_err(map_db)?;
        let result=sqlx::query("INSERT INTO litecr.catalog_source(registry_id,organization_id,namespace,message_key,source_text,source_revision,placeholders,plural) VALUES($1,$2,$3,$4,$5,$6,$7,$8) ON CONFLICT(registry_id,organization_id,namespace,message_key) DO UPDATE SET source_text=EXCLUDED.source_text,source_revision=EXCLUDED.source_revision,placeholders=EXCLUDED.placeholders,plural=EXCLUDED.plural,updated_at=now() WHERE litecr.catalog_source.source_revision < EXCLUDED.source_revision").bind(r).bind(o).bind(&x.namespace).bind(&x.key).bind(&x.source_text).bind(x.source_revision).bind(x.placeholders.iter().cloned().collect::<Vec<_>>()).bind(x.plural).execute(&mut *tx).await.map_err(map_db)?;
        if result.rows_affected() != 1 {
            return Err(CatalogError::Conflict);
        }
        audit(
            &mut tx,
            r,
            o,
            a,
            "source.update",
            &format!("{}.{}", x.namespace, x.key),
            Some(x.source_revision),
        )
        .await?;
        tx.commit().await.map_err(map_db)
    }
    async fn save_draft(
        &self,
        s: &litecr_application::access::ResourceScope,
        x: &DraftTranslation,
        a: &str,
    ) -> Result<(), CatalogError> {
        let (r, o) = scope_bind(s);
        let mut tx = self.pool.begin().await.map_err(map_db)?;
        let current=sqlx::query("SELECT source_revision,placeholders,plural FROM litecr.catalog_source WHERE registry_id=$1 AND organization_id=$2 AND namespace=$3 AND message_key=$4 FOR UPDATE").bind(r).bind(o).bind(&x.namespace).bind(&x.key).fetch_optional(&mut *tx).await.map_err(map_db)?;
        let Some(current) = current else {
            return Err(CatalogError::NotFound);
        };
        let current_revision: i64 = current.try_get("source_revision").map_err(map_db)?;
        let source = MessageSource {
            namespace: x.namespace.clone(),
            key: x.key.clone(),
            source_text: String::new(),
            source_revision: current_revision,
            placeholders: current
                .try_get::<Vec<String>, _>("placeholders")
                .map_err(map_db)?
                .into_iter()
                .collect(),
            plural: current.try_get("plural").map_err(map_db)?,
        };
        if current_revision != x.source_revision
            || !litecr_application::localization::valid_translation(&source, x)
        {
            return Err(CatalogError::Conflict);
        }
        let forms = x.plural_forms.as_ref().map(|forms| json!(forms));
        let old:Option<String>=sqlx::query_scalar("SELECT edited_by FROM litecr.catalog_translation_draft WHERE registry_id=$1 AND organization_id=$2 AND locale_tag=$3 AND namespace=$4 AND message_key=$5 FOR UPDATE").bind(r).bind(o).bind(&x.locale).bind(&x.namespace).bind(&x.key).fetch_optional(&mut *tx).await.map_err(map_db)?;
        if old.is_some_and(|v| v == a) {
            sqlx::query("UPDATE litecr.catalog_translation_draft SET translation_text=$6,plural_forms=$7,source_revision=$8,state='draft',edited_by=$9,updated_at=now() WHERE registry_id=$1 AND organization_id=$2 AND locale_tag=$3 AND namespace=$4 AND message_key=$5").bind(r).bind(o).bind(&x.locale).bind(&x.namespace).bind(&x.key).bind(&x.value).bind(forms).bind(x.source_revision).bind(a).execute(&mut *tx).await.map_err(map_db)?;
        } else {
            sqlx::query("INSERT INTO litecr.catalog_translation_draft(registry_id,organization_id,locale_tag,namespace,message_key,translation_text,plural_forms,source_revision,state,edited_by) VALUES($1,$2,$3,$4,$5,$6,$7,$8,'draft',$9) ON CONFLICT(registry_id,organization_id,locale_tag,namespace,message_key) DO UPDATE SET translation_text=EXCLUDED.translation_text,plural_forms=EXCLUDED.plural_forms,source_revision=EXCLUDED.source_revision,state='draft',edited_by=EXCLUDED.edited_by,updated_at=now()").bind(r).bind(o).bind(&x.locale).bind(&x.namespace).bind(&x.key).bind(&x.value).bind(forms).bind(x.source_revision).bind(a).execute(&mut *tx).await.map_err(map_db)?;
        }
        audit(
            &mut tx,
            r,
            o,
            a,
            "translation.draft.save",
            &format!("{}.{}.{}", x.locale, x.namespace, x.key),
            Some(x.source_revision),
        )
        .await?;
        tx.commit().await.map_err(map_db)
    }
    async fn load_draft(
        &self,
        s: &litecr_application::access::ResourceScope,
        l: &str,
        n: &str,
        k: &str,
    ) -> Result<Option<DraftTranslation>, CatalogError> {
        let (r, o) = scope_bind(s);
        let row=sqlx::query("SELECT d.locale_tag,d.namespace,d.message_key,d.translation_text,d.plural_forms,d.source_revision,d.state,s.source_revision AS current_source_revision FROM litecr.catalog_translation_draft d JOIN litecr.catalog_source s USING(registry_id,organization_id,namespace,message_key) WHERE d.registry_id=$1 AND d.organization_id=$2 AND d.locale_tag=$3 AND d.namespace=$4 AND d.message_key=$5").bind(r).bind(o).bind(l).bind(n).bind(k).fetch_optional(&self.pool).await.map_err(map_db)?;
        row.as_ref().map(draft).transpose()
    }
    async fn load_source(
        &self,
        s: &litecr_application::access::ResourceScope,
        n: &str,
        k: &str,
    ) -> Result<Option<MessageSource>, CatalogError> {
        let (r, o) = scope_bind(s);
        let row=sqlx::query("SELECT namespace,message_key,source_text,source_revision,placeholders,plural FROM litecr.catalog_source WHERE registry_id=$1 AND organization_id=$2 AND namespace=$3 AND message_key=$4").bind(r).bind(o).bind(n).bind(k).fetch_optional(&self.pool).await.map_err(map_db)?;
        row.map(|x| {
            Ok(MessageSource {
                namespace: x
                    .try_get("namespace")
                    .map_err(|_| CatalogError::Unavailable)?,
                key: x
                    .try_get("message_key")
                    .map_err(|_| CatalogError::Unavailable)?,
                source_text: x
                    .try_get("source_text")
                    .map_err(|_| CatalogError::Unavailable)?,
                source_revision: x
                    .try_get("source_revision")
                    .map_err(|_| CatalogError::Unavailable)?,
                placeholders: x
                    .try_get::<Vec<String>, _>("placeholders")
                    .map_err(|_| CatalogError::Unavailable)?
                    .into_iter()
                    .collect(),
                plural: x.try_get("plural").map_err(|_| CatalogError::Unavailable)?,
            })
        })
        .transpose()
    }
    async fn list_entries(
        &self,
        s: &litecr_application::access::ResourceScope,
        l: &str,
        n: &str,
    ) -> Result<Vec<CatalogEntry>, CatalogError> {
        let (r, o) = scope_bind(s);
        let rows = sqlx::query("SELECT s.namespace,s.message_key,s.source_text,s.source_revision,s.placeholders,s.plural,d.locale_tag,d.namespace AS draft_namespace,d.message_key AS draft_key,d.translation_text,d.plural_forms,d.source_revision AS draft_source_revision,d.state,d.source_revision<>s.source_revision AS stale,s.source_revision AS current_source_revision FROM litecr.catalog_source s LEFT JOIN litecr.catalog_translation_draft d ON d.registry_id=s.registry_id AND d.organization_id=s.organization_id AND d.namespace=s.namespace AND d.message_key=s.message_key AND d.locale_tag=$3 WHERE s.registry_id=$1 AND s.organization_id=$2 AND s.namespace=$4 ORDER BY s.message_key LIMIT 10000")
            .bind(r).bind(o).bind(l).bind(n).fetch_all(&self.pool).await.map_err(map_db)?;
        rows.iter()
            .map(|row| {
                let source = MessageSource {
                    namespace: row
                        .try_get("namespace")
                        .map_err(|_| CatalogError::Unavailable)?,
                    key: row
                        .try_get("message_key")
                        .map_err(|_| CatalogError::Unavailable)?,
                    source_text: row
                        .try_get("source_text")
                        .map_err(|_| CatalogError::Unavailable)?,
                    source_revision: row
                        .try_get("source_revision")
                        .map_err(|_| CatalogError::Unavailable)?,
                    placeholders: row
                        .try_get::<Vec<String>, _>("placeholders")
                        .map_err(|_| CatalogError::Unavailable)?
                        .into_iter()
                        .collect(),
                    plural: row
                        .try_get("plural")
                        .map_err(|_| CatalogError::Unavailable)?,
                };
                let draft_locale: Option<String> = row
                    .try_get("locale_tag")
                    .map_err(|_| CatalogError::Unavailable)?;
                let draft = if draft_locale.is_some() {
                    let translation_text: String = row
                        .try_get("translation_text")
                        .map_err(|_| CatalogError::Unavailable)?;
                    let forms: Option<Value> = row
                        .try_get("plural_forms")
                        .map_err(|_| CatalogError::Unavailable)?;
                    Some(DraftTranslation {
                        locale: draft_locale.unwrap_or_default(),
                        namespace: row
                            .try_get("draft_namespace")
                            .map_err(|_| CatalogError::Unavailable)?,
                        key: row
                            .try_get("draft_key")
                            .map_err(|_| CatalogError::Unavailable)?,
                        value: translation_text,
                        plural_forms: forms
                            .map(serde_json::from_value)
                            .transpose()
                            .map_err(|_| CatalogError::Unavailable)?,
                        source_revision: row
                            .try_get("draft_source_revision")
                            .map_err(|_| CatalogError::Unavailable)?,
                        state: state(
                            &row.try_get::<String, _>("state")
                                .map_err(|_| CatalogError::Unavailable)?,
                        )?,
                        stale: row
                            .try_get("stale")
                            .map_err(|_| CatalogError::Unavailable)?,
                    })
                } else {
                    None
                };
                Ok(CatalogEntry { source, draft })
            })
            .collect()
    }
    async fn review(
        &self,
        s: &litecr_application::access::ResourceScope,
        l: &str,
        n: &str,
        k: &str,
        d: ReviewDecision,
        a: &str,
    ) -> Result<(), CatalogError> {
        let (r, o) = scope_bind(s);
        let state = if d == ReviewDecision::Approve {
            "approved"
        } else {
            "draft"
        };
        let mut tx = self.pool.begin().await.map_err(map_db)?;
        let editor:Option<String>=sqlx::query_scalar("SELECT edited_by FROM litecr.catalog_translation_draft WHERE registry_id=$1 AND organization_id=$2 AND locale_tag=$3 AND namespace=$4 AND message_key=$5 AND source_revision=(SELECT source_revision FROM litecr.catalog_source WHERE registry_id=$1 AND organization_id=$2 AND namespace=$4 AND message_key=$5) FOR UPDATE").bind(r).bind(o).bind(l).bind(n).bind(k).fetch_optional(&mut *tx).await.map_err(map_db)?;
        let editor = editor.ok_or(CatalogError::NotFound)?;
        if editor == a {
            return Err(CatalogError::Denied(
                litecr_application::access::AccessDenied::PermissionMissing,
            ));
        }
        let result=sqlx::query("UPDATE litecr.catalog_translation_draft SET state=$6,updated_at=now() WHERE registry_id=$1 AND organization_id=$2 AND locale_tag=$3 AND namespace=$4 AND message_key=$5").bind(r).bind(o).bind(l).bind(n).bind(k).bind(state).execute(&mut *tx).await.map_err(map_db)?;
        if result.rows_affected() != 1 {
            return Err(CatalogError::NotFound);
        }
        audit(
            &mut tx,
            r,
            o,
            a,
            "translation.review",
            &format!("{l}.{n}.{k}"),
            None,
        )
        .await?;
        tx.commit().await.map_err(map_db)
    }
    async fn publish(
        &self,
        s: &litecr_application::access::ResourceScope,
        l: &str,
        n: &str,
        expected: Option<i64>,
        a: &str,
    ) -> Result<PublishedBundle, CatalogError> {
        let (r, o) = scope_bind(s);
        let mut tx = self.pool.begin().await.map_err(map_db)?;
        let bundle = publish_bundle(
            &mut tx,
            BundlePublish {
                registry_id: r,
                organization_id: o,
                locale: l,
                namespace: n,
                expected_revision: expected,
                actor: a,
                rollback_revision: None,
            },
        )
        .await?;
        tx.commit().await.map_err(map_db)?;
        Ok(bundle)
    }
    async fn rollback(
        &self,
        s: &litecr_application::access::ResourceScope,
        l: &str,
        n: &str,
        target: i64,
        expected: Option<i64>,
        a: &str,
    ) -> Result<PublishedBundle, CatalogError> {
        let (r, o) = scope_bind(s);
        let mut tx = self.pool.begin().await.map_err(map_db)?;
        let bundle = publish_bundle(
            &mut tx,
            BundlePublish {
                registry_id: r,
                organization_id: o,
                locale: l,
                namespace: n,
                expected_revision: expected,
                actor: a,
                rollback_revision: Some(target),
            },
        )
        .await?;
        tx.commit().await.map_err(map_db)?;
        Ok(bundle)
    }
    async fn load_bundle(
        &self,
        s: &litecr_application::access::ResourceScope,
        l: &str,
        n: &str,
    ) -> Result<Option<PublishedBundle>, CatalogError> {
        let (r, o) = scope_bind(s);
        let row=sqlx::query("SELECT r.bundle,r.revision FROM litecr.catalog_bundle_active a JOIN litecr.catalog_bundle_release r ON r.registry_id=a.registry_id AND r.organization_id=a.organization_id AND r.locale_tag=a.locale_tag AND r.namespace=a.namespace AND r.revision=a.active_revision WHERE a.registry_id=$1 AND a.organization_id=$2 AND a.locale_tag=$3 AND a.namespace=$4").bind(r).bind(o).bind(l).bind(n).fetch_optional(&self.pool).await.map_err(map_db)?;
        row.map(|x| {
            let raw: Value = x.try_get("bundle").map_err(|_| CatalogError::Unavailable)?;
            let entries = serde_json::from_value::<BTreeMap<String, String>>(raw)
                .map_err(|_| CatalogError::Unavailable)?;
            Ok(PublishedBundle {
                locale: l.to_owned(),
                namespace: n.to_owned(),
                revision: x
                    .try_get("revision")
                    .map_err(|_| CatalogError::Unavailable)?,
                entries,
                fallback_locale: "en".into(),
            })
        })
        .transpose()
    }
}

struct BundlePublish<'a> {
    registry_id: &'a str,
    organization_id: &'a str,
    locale: &'a str,
    namespace: &'a str,
    expected_revision: Option<i64>,
    actor: &'a str,
    rollback_revision: Option<i64>,
}

async fn publish_bundle(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    command: BundlePublish<'_>,
) -> Result<PublishedBundle, CatalogError> {
    let BundlePublish {
        registry_id: r,
        organization_id: o,
        locale: l,
        namespace: n,
        expected_revision: expected,
        actor,
        rollback_revision: rollback,
    } = command;
    let active:Option<i64>=sqlx::query_scalar("SELECT active_revision FROM litecr.catalog_bundle_active WHERE registry_id=$1 AND organization_id=$2 AND locale_tag=$3 AND namespace=$4 FOR UPDATE").bind(r).bind(o).bind(l).bind(n).fetch_optional(&mut **tx).await.map_err(map_db)?;
    if active != expected {
        return Err(CatalogError::Conflict);
    }
    let (entries, source_revisions) = if let Some(target) = rollback {
        let row=sqlx::query("SELECT bundle,source_revisions FROM litecr.catalog_bundle_release WHERE registry_id=$1 AND organization_id=$2 AND locale_tag=$3 AND namespace=$4 AND revision=$5").bind(r).bind(o).bind(l).bind(n).bind(target).fetch_optional(&mut **tx).await.map_err(map_db)?.ok_or(CatalogError::NotFound)?;
        (
            row.try_get::<Value, _>("bundle").map_err(map_db)?,
            row.try_get::<Value, _>("source_revisions")
                .map_err(map_db)?,
        )
    } else {
        let rows=sqlx::query("SELECT d.message_key,d.translation_text,d.plural_forms,d.source_revision,d.state,s.source_revision AS current_source_revision,s.placeholders,s.plural,s.source_text FROM litecr.catalog_translation_draft d JOIN litecr.catalog_source s USING(registry_id,organization_id,namespace,message_key) WHERE d.registry_id=$1 AND d.organization_id=$2 AND d.locale_tag=$3 AND d.namespace=$4 ORDER BY d.message_key FOR UPDATE OF d,s").bind(r).bind(o).bind(l).bind(n).fetch_all(&mut **tx).await.map_err(map_db)?;
        if rows.is_empty() {
            return Err(CatalogError::Invalid);
        }
        let mut bundle = BTreeMap::new();
        let mut versions = BTreeMap::new();
        for row in rows {
            let key: String = row.try_get("message_key").map_err(map_db)?;
            let text: String = row.try_get("translation_text").map_err(map_db)?;
            let rev: i64 = row.try_get("source_revision").map_err(map_db)?;
            let state: String = row.try_get("state").map_err(map_db)?;
            let current: i64 = row.try_get("current_source_revision").map_err(map_db)?;
            let ph: Vec<String> = row.try_get("placeholders").map_err(map_db)?;
            let plural: bool = row.try_get("plural").map_err(map_db)?;
            let src: String = row.try_get("source_text").map_err(map_db)?;
            let source = MessageSource {
                namespace: n.to_owned(),
                key: key.clone(),
                source_text: src,
                source_revision: current,
                placeholders: ph.into_iter().collect(),
                plural,
            };
            let form_json: Option<Value> = row.try_get("plural_forms").map_err(map_db)?;
            let forms: Option<BTreeMap<String, String>> = form_json
                .map(serde_json::from_value)
                .transpose()
                .map_err(|_| CatalogError::Unavailable)?;
            if state != "approved" || rev != current {
                return Err(CatalogError::Conflict);
            }
            if source.plural {
                let forms = forms.ok_or(CatalogError::Conflict)?;
                if !valid_plural_forms(l, &forms)
                    || !valid_plural_values(&source, Some(&forms))
                    || text != forms.get("other").cloned().unwrap_or_default()
                {
                    return Err(CatalogError::Conflict);
                }
                for (category, value) in forms {
                    let bundle_key = format!("{key}_{category}");
                    versions.insert(bundle_key.clone(), json!(current));
                    bundle.insert(bundle_key, value);
                }
            } else {
                if forms.is_some()
                    || text.contains(['<', '>'])
                    || placeholders(&text) != source.placeholders
                {
                    return Err(CatalogError::Conflict);
                }
                versions.insert(key.clone(), json!(current));
                bundle.insert(key.clone(), text);
            }
        }
        (json!(bundle), json!(versions))
    };
    let revision:i64=sqlx::query_scalar("SELECT COALESCE(MAX(revision),0)+1 FROM litecr.catalog_bundle_release WHERE registry_id=$1 AND organization_id=$2 AND locale_tag=$3 AND namespace=$4").bind(r).bind(o).bind(l).bind(n).fetch_one(&mut **tx).await.map_err(map_db)?;
    sqlx::query("INSERT INTO litecr.catalog_bundle_release(registry_id,organization_id,locale_tag,namespace,revision,source_revisions,bundle,published_by) VALUES($1,$2,$3,$4,$5,$6,$7,$8)").bind(r).bind(o).bind(l).bind(n).bind(revision).bind(source_revisions).bind(&entries).bind(actor).execute(&mut **tx).await.map_err(map_db)?;
    sqlx::query("INSERT INTO litecr.catalog_bundle_active(registry_id,organization_id,locale_tag,namespace,active_revision) VALUES($1,$2,$3,$4,$5) ON CONFLICT(registry_id,organization_id,locale_tag,namespace) DO UPDATE SET active_revision=EXCLUDED.active_revision").bind(r).bind(o).bind(l).bind(n).bind(revision).execute(&mut **tx).await.map_err(map_db)?;
    audit(
        tx,
        r,
        o,
        actor,
        if rollback.is_some() {
            "translation.rollback"
        } else {
            "translation.publish"
        },
        &format!("{l}.{n}"),
        Some(revision),
    )
    .await?;
    let entries = serde_json::from_value::<BTreeMap<String, String>>(entries)
        .map_err(|_| CatalogError::Unavailable)?;
    Ok(PublishedBundle {
        locale: l.to_owned(),
        namespace: n.to_owned(),
        revision,
        entries,
        fallback_locale: "en".into(),
    })
}
async fn audit(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    r: &str,
    o: &str,
    a: &str,
    action: &str,
    key: &str,
    version: Option<i64>,
) -> Result<(), CatalogError> {
    sqlx::query("INSERT INTO litecr.configuration_audit(registry_id,organization_id,actor_ref,action,resource_key,version) VALUES($1,$2,$3,$4,$5,$6)").bind(r).bind(o).bind(a).bind(action).bind(key).bind(version).execute(&mut **tx).await.map_err(map_db)?;
    Ok(())
}
