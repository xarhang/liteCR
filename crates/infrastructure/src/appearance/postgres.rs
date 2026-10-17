use async_trait::async_trait;
use litecr_application::{
    access::ResourceScope,
    appearance::{
        AppearanceError, AppearancePreference, AppearanceProfile, AppearanceRepository, ColorMode,
        Density, SemanticTokens,
    },
};
use serde_json::{Value, json};
use sqlx::{PgPool, Row};

#[derive(Clone)]
pub struct PostgresAppearanceRepository {
    pool: PgPool,
}
impl PostgresAppearanceRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}
fn err(e: sqlx::Error) -> AppearanceError {
    if matches!(e, sqlx::Error::RowNotFound) {
        AppearanceError::NotFound
    } else if matches!(e,sqlx::Error::Database(ref d) if d.code().as_deref()==Some("23505")||d.code().as_deref()==Some("40001"))
    {
        AppearanceError::Conflict
    } else {
        AppearanceError::Unavailable
    }
}
fn encode(p: &AppearanceProfile) -> Value {
    json!({"name":p.name,"accent":p.accent,"on_accent":p.on_accent,"logo_asset_ref":p.logo_asset_ref,"color_mode":match p.color_mode{ColorMode::Light=>"light",ColorMode::Dark=>"dark",ColorMode::System=>"system"},"density":match p.density{Density::Comfortable=>"comfortable",Density::Compact=>"compact"},"high_contrast":p.high_contrast,"tokens":{"canvas":p.tokens.canvas,"surface":p.tokens.surface,"text":p.tokens.text,"text_muted":p.tokens.text_muted,"action":p.tokens.action,"on_action":p.tokens.on_action,"control_border":p.tokens.control_border,"focus":p.tokens.focus}})
}
fn decode(version: i64, v: Value) -> Result<AppearanceProfile, AppearanceError> {
    Ok(AppearanceProfile {
        name: v["name"]
            .as_str()
            .ok_or(AppearanceError::Unavailable)?
            .into(),
        accent: v["accent"]
            .as_str()
            .ok_or(AppearanceError::Unavailable)?
            .into(),
        on_accent: v["on_accent"]
            .as_str()
            .ok_or(AppearanceError::Unavailable)?
            .into(),
        logo_asset_ref: serde_json::from_value(v["logo_asset_ref"].clone())
            .map_err(|_| AppearanceError::Unavailable)?,
        color_mode: match v["color_mode"].as_str() {
            Some("light") => ColorMode::Light,
            Some("dark") => ColorMode::Dark,
            Some("system") => ColorMode::System,
            _ => return Err(AppearanceError::Unavailable),
        },
        density: match v["density"].as_str() {
            Some("comfortable") => Density::Comfortable,
            Some("compact") => Density::Compact,
            _ => return Err(AppearanceError::Unavailable),
        },
        high_contrast: v["high_contrast"]
            .as_bool()
            .ok_or(AppearanceError::Unavailable)?,
        tokens: SemanticTokens {
            canvas: v["tokens"]["canvas"]
                .as_str()
                .ok_or(AppearanceError::Unavailable)?
                .into(),
            surface: v["tokens"]["surface"]
                .as_str()
                .ok_or(AppearanceError::Unavailable)?
                .into(),
            text: v["tokens"]["text"]
                .as_str()
                .ok_or(AppearanceError::Unavailable)?
                .into(),
            text_muted: v["tokens"]["text_muted"]
                .as_str()
                .ok_or(AppearanceError::Unavailable)?
                .into(),
            action: v["tokens"]["action"]
                .as_str()
                .ok_or(AppearanceError::Unavailable)?
                .into(),
            on_action: v["tokens"]["on_action"]
                .as_str()
                .ok_or(AppearanceError::Unavailable)?
                .into(),
            control_border: v["tokens"]["control_border"]
                .as_str()
                .ok_or(AppearanceError::Unavailable)?
                .into(),
            focus: v["tokens"]["focus"]
                .as_str()
                .ok_or(AppearanceError::Unavailable)?
                .into(),
        },
        version,
    })
}
async fn audit(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    s: &ResourceScope,
    a: &str,
    action: &str,
    v: i64,
) -> Result<(), AppearanceError> {
    sqlx::query("INSERT INTO litecr.configuration_audit(registry_id,organization_id,actor_ref,action,resource_key,version) VALUES($1,$2,$3,$4,'appearance',$5)").bind(&s.registry_id).bind(&s.organization_id).bind(a).bind(action).bind(v).execute(&mut **tx).await.map_err(err)?;
    Ok(())
}
#[async_trait]
impl AppearanceRepository for PostgresAppearanceRepository {
    async fn save_draft(
        &self,
        s: &ResourceScope,
        p: &AppearanceProfile,
        expected: Option<i64>,
        a: &str,
    ) -> Result<(), AppearanceError> {
        let mut tx = self.pool.begin().await.map_err(err)?;
        sqlx::query(
            "SELECT pg_advisory_xact_lock(hashtextextended($1 || ':' || $2 || ':appearance',0))",
        )
        .bind(&s.registry_id)
        .bind(&s.organization_id)
        .execute(&mut *tx)
        .await
        .map_err(err)?;
        let latest:Option<i64>=sqlx::query_scalar("SELECT MAX(version) FROM litecr.appearance_version WHERE registry_id=$1 AND organization_id=$2").bind(&s.registry_id).bind(&s.organization_id).fetch_one(&mut *tx).await.map_err(err)?;
        let actual = latest.unwrap_or(0);
        if expected.is_some_and(|v| v != actual) {
            return Err(AppearanceError::Conflict);
        }
        let version = actual + 1;
        sqlx::query("INSERT INTO litecr.appearance_version(registry_id,organization_id,version,config,state,updated_by) VALUES($1,$2,$3,$4,'draft',$5)").bind(&s.registry_id).bind(&s.organization_id).bind(version).bind(encode(p)).bind(a).execute(&mut *tx).await.map_err(err)?;
        audit(&mut tx, s, a, "appearance.draft.save", version).await?;
        tx.commit().await.map_err(err)
    }
    async fn publish(
        &self,
        s: &ResourceScope,
        expected: i64,
        a: &str,
    ) -> Result<AppearanceProfile, AppearanceError> {
        let mut tx = self.pool.begin().await.map_err(err)?;
        let active:Option<i64>=sqlx::query_scalar("SELECT active_version FROM litecr.appearance_active WHERE registry_id=$1 AND organization_id=$2 FOR UPDATE").bind(&s.registry_id).bind(&s.organization_id).fetch_optional(&mut *tx).await.map_err(err)?;
        let latest:i64=sqlx::query_scalar("SELECT COALESCE(MAX(version),0) FROM litecr.appearance_version WHERE registry_id=$1 AND organization_id=$2 AND state='draft'").bind(&s.registry_id).bind(&s.organization_id).fetch_one(&mut *tx).await.map_err(err)?;
        if active.unwrap_or(0) != expected || latest <= 0 {
            return Err(AppearanceError::Conflict);
        }
        let config:Value=sqlx::query_scalar("UPDATE litecr.appearance_version SET state='published',updated_by=$3,updated_at=now() WHERE registry_id=$1 AND organization_id=$2 AND version=$4 AND state='draft' RETURNING config").bind(&s.registry_id).bind(&s.organization_id).bind(a).bind(latest).fetch_optional(&mut *tx).await.map_err(err)?.ok_or(AppearanceError::Conflict)?;
        sqlx::query("INSERT INTO litecr.appearance_active(registry_id,organization_id,active_version) VALUES($1,$2,$3) ON CONFLICT(registry_id,organization_id) DO UPDATE SET active_version=EXCLUDED.active_version").bind(&s.registry_id).bind(&s.organization_id).bind(latest).execute(&mut *tx).await.map_err(err)?;
        audit(&mut tx, s, a, "appearance.publish", latest).await?;
        tx.commit().await.map_err(err)?;
        decode(latest, config)
    }
    async fn rollback(
        &self,
        s: &ResourceScope,
        target: i64,
        expected: i64,
        a: &str,
    ) -> Result<AppearanceProfile, AppearanceError> {
        let mut tx = self.pool.begin().await.map_err(err)?;
        let active:Option<i64>=sqlx::query_scalar("SELECT active_version FROM litecr.appearance_active WHERE registry_id=$1 AND organization_id=$2 FOR UPDATE").bind(&s.registry_id).bind(&s.organization_id).fetch_optional(&mut *tx).await.map_err(err)?;
        if active != Some(expected) {
            return Err(AppearanceError::Conflict);
        }
        let config:Value=sqlx::query_scalar("SELECT config FROM litecr.appearance_version WHERE registry_id=$1 AND organization_id=$2 AND version=$3 AND state='published'").bind(&s.registry_id).bind(&s.organization_id).bind(target).fetch_optional(&mut *tx).await.map_err(err)?.ok_or(AppearanceError::NotFound)?;
        let version:i64=sqlx::query_scalar("SELECT COALESCE(MAX(version),0)+1 FROM litecr.appearance_version WHERE registry_id=$1 AND organization_id=$2").bind(&s.registry_id).bind(&s.organization_id).fetch_one(&mut *tx).await.map_err(err)?;
        sqlx::query("INSERT INTO litecr.appearance_version(registry_id,organization_id,version,config,state,updated_by) VALUES($1,$2,$3,$4,'published',$5)").bind(&s.registry_id).bind(&s.organization_id).bind(version).bind(&config).bind(a).execute(&mut *tx).await.map_err(err)?;
        sqlx::query("UPDATE litecr.appearance_active SET active_version=$3 WHERE registry_id=$1 AND organization_id=$2").bind(&s.registry_id).bind(&s.organization_id).bind(version).execute(&mut *tx).await.map_err(err)?;
        audit(&mut tx, s, a, "appearance.rollback", version).await?;
        tx.commit().await.map_err(err)?;
        decode(version, config)
    }
    async fn current(
        &self,
        s: &ResourceScope,
    ) -> Result<Option<AppearanceProfile>, AppearanceError> {
        let row=sqlx::query("SELECT v.config,v.version FROM litecr.appearance_active a JOIN litecr.appearance_version v ON v.registry_id=a.registry_id AND v.organization_id=a.organization_id AND v.version=a.active_version WHERE a.registry_id=$1 AND a.organization_id=$2").bind(&s.registry_id).bind(&s.organization_id).fetch_optional(&self.pool).await.map_err(err)?;
        row.map(|r| {
            decode(
                r.try_get("version").map_err(err)?,
                r.try_get("config").map_err(err)?,
            )
        })
        .transpose()
    }
    async fn save_preference(
        &self,
        s: &ResourceScope,
        actor: &str,
        p: &AppearancePreference,
    ) -> Result<(), AppearanceError> {
        let color = p.color_mode.map(|v| match v {
            ColorMode::Light => "light",
            ColorMode::Dark => "dark",
            ColorMode::System => "system",
        });
        let density = p.density.map(|v| match v {
            Density::Comfortable => "comfortable",
            Density::Compact => "compact",
        });
        let mut tx = self.pool.begin().await.map_err(err)?;
        sqlx::query("INSERT INTO litecr.appearance_preference(registry_id,organization_id,actor_ref,color_mode,density,high_contrast,locale_tag) VALUES($1,$2,$3,$4,$5,$6,$7) ON CONFLICT(registry_id,organization_id,actor_ref) DO UPDATE SET color_mode=EXCLUDED.color_mode,density=EXCLUDED.density,high_contrast=EXCLUDED.high_contrast,locale_tag=EXCLUDED.locale_tag,updated_at=now()")
            .bind(&s.registry_id).bind(&s.organization_id).bind(actor).bind(color).bind(density).bind(p.high_contrast).bind(&p.locale_tag).execute(&mut *tx).await.map_err(err)?;
        audit(&mut tx, s, actor, "appearance.preference.update", 0).await?;
        tx.commit().await.map_err(err)
    }
    async fn preference(
        &self,
        s: &ResourceScope,
        actor: &str,
    ) -> Result<Option<AppearancePreference>, AppearanceError> {
        let row=sqlx::query("SELECT color_mode,density,high_contrast,locale_tag FROM litecr.appearance_preference WHERE registry_id=$1 AND organization_id=$2 AND actor_ref=$3")
            .bind(&s.registry_id).bind(&s.organization_id).bind(actor).fetch_optional(&self.pool).await.map_err(err)?;
        row.map(|r| {
            let color: Option<String> = r.try_get("color_mode").map_err(err)?;
            let density: Option<String> = r.try_get("density").map_err(err)?;
            Ok(AppearancePreference {
                color_mode: color
                    .map(|v| match v.as_str() {
                        "light" => Ok(ColorMode::Light),
                        "dark" => Ok(ColorMode::Dark),
                        "system" => Ok(ColorMode::System),
                        _ => Err(AppearanceError::Unavailable),
                    })
                    .transpose()?,
                density: density
                    .map(|v| match v.as_str() {
                        "comfortable" => Ok(Density::Comfortable),
                        "compact" => Ok(Density::Compact),
                        _ => Err(AppearanceError::Unavailable),
                    })
                    .transpose()?,
                high_contrast: r.try_get("high_contrast").map_err(err)?,
                locale_tag: r.try_get("locale_tag").map_err(err)?,
            })
        })
        .transpose()
    }
}
