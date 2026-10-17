use async_trait::async_trait;
use litecr_application::{
    access::ResourceScope,
    country_profiles::{
        CountryProfile, CountryProfileCoverage, CountryProfileRepository, ProfileError,
    },
};
use serde_json::{Value, json};
use sqlx::{PgPool, Row};

#[derive(Clone)]
pub struct PostgresCountryProfileRepository {
    pool: PgPool,
}
impl PostgresCountryProfileRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}
fn err(e: sqlx::Error) -> ProfileError {
    if matches!(e, sqlx::Error::RowNotFound) {
        ProfileError::NotFound
    } else if matches!(e,sqlx::Error::Database(ref d) if d.code().as_deref()==Some("23505")||d.code().as_deref()==Some("40001"))
    {
        ProfileError::Conflict
    } else {
        ProfileError::Unavailable
    }
}
fn encode(p: &CountryProfile) -> Value {
    json!({"default_locale":p.default_locale,"calendar":p.calendar,"timezone":p.timezone,"address_fields":p.address_fields,"identifier_types":p.identifier_types,"phone_region":p.phone_region})
}
fn decode(country: String, version: i64, v: Value) -> Result<CountryProfile, ProfileError> {
    Ok(CountryProfile {
        country_code: country,
        default_locale: v
            .get("default_locale")
            .and_then(Value::as_str)
            .ok_or(ProfileError::Unavailable)?
            .to_owned(),
        calendar: v
            .get("calendar")
            .and_then(Value::as_str)
            .ok_or(ProfileError::Unavailable)?
            .to_owned(),
        timezone: v
            .get("timezone")
            .and_then(Value::as_str)
            .ok_or(ProfileError::Unavailable)?
            .to_owned(),
        address_fields: serde_json::from_value(
            v.get("address_fields")
                .cloned()
                .ok_or(ProfileError::Unavailable)?,
        )
        .map_err(|_| ProfileError::Unavailable)?,
        identifier_types: serde_json::from_value(
            v.get("identifier_types")
                .cloned()
                .ok_or(ProfileError::Unavailable)?,
        )
        .map_err(|_| ProfileError::Unavailable)?,
        phone_region: v
            .get("phone_region")
            .and_then(Value::as_str)
            .ok_or(ProfileError::Unavailable)?
            .to_owned(),
        matching_policy_ref: None,
        version,
    })
}
#[async_trait]
impl CountryProfileRepository for PostgresCountryProfileRepository {
    async fn coverage(
        &self,
        s: &ResourceScope,
    ) -> Result<Vec<CountryProfileCoverage>, ProfileError> {
        let rows = sqlx::query(
            "SELECT v.country_code, MAX(v.version) AS latest_version, a.active_version \
             FROM litecr.country_profile_version v \
             LEFT JOIN litecr.country_profile_active a \
               ON a.registry_id=v.registry_id AND a.organization_id=v.organization_id \
              AND a.country_code=v.country_code \
             WHERE v.registry_id=$1 AND v.organization_id=$2 \
             GROUP BY v.country_code, a.active_version ORDER BY v.country_code",
        )
        .bind(&s.registry_id)
        .bind(&s.organization_id)
        .fetch_all(&self.pool)
        .await
        .map_err(err)?;
        rows.into_iter()
            .map(|row| {
                Ok(CountryProfileCoverage {
                    country_code: row.try_get("country_code").map_err(err)?,
                    latest_version: row.try_get("latest_version").map_err(err)?,
                    active_version: row.try_get("active_version").map_err(err)?,
                })
            })
            .collect()
    }
    async fn save_draft(
        &self,
        s: &ResourceScope,
        p: &CountryProfile,
        expected: Option<i64>,
        a: &str,
    ) -> Result<(), ProfileError> {
        let mut tx = self.pool.begin().await.map_err(err)?;
        sqlx::query(
            "SELECT pg_advisory_xact_lock(hashtextextended($1 || ':' || $2 || ':' || $3,0))",
        )
        .bind(&s.registry_id)
        .bind(&s.organization_id)
        .bind(&p.country_code)
        .execute(&mut *tx)
        .await
        .map_err(err)?;
        let latest:Option<i64>=sqlx::query_scalar("SELECT MAX(version) FROM litecr.country_profile_version WHERE registry_id=$1 AND organization_id=$2 AND country_code=$3").bind(&s.registry_id).bind(&s.organization_id).bind(&p.country_code).fetch_one(&mut *tx).await.map_err(err)?;
        let actual = latest.unwrap_or(0);
        if expected.is_some_and(|v| v != actual) {
            return Err(ProfileError::Conflict);
        }
        let version = actual + 1;
        sqlx::query("INSERT INTO litecr.country_profile_version(registry_id,organization_id,country_code,version,config,state,updated_by) VALUES($1,$2,$3,$4,$5,'draft',$6)").bind(&s.registry_id).bind(&s.organization_id).bind(&p.country_code).bind(version).bind(encode(p)).bind(a).execute(&mut *tx).await.map_err(err)?;
        sqlx::query("INSERT INTO litecr.configuration_audit(registry_id,organization_id,actor_ref,action,resource_key,version) VALUES($1,$2,$3,'country-profile.draft.save',$4,$5)").bind(&s.registry_id).bind(&s.organization_id).bind(a).bind(&p.country_code).bind(version).execute(&mut *tx).await.map_err(err)?;
        tx.commit().await.map_err(err)
    }
    async fn publish(
        &self,
        s: &ResourceScope,
        country: &str,
        expected: i64,
        a: &str,
    ) -> Result<CountryProfile, ProfileError> {
        let mut tx = self.pool.begin().await.map_err(err)?;
        let row=sqlx::query("SELECT config,state FROM litecr.country_profile_version WHERE registry_id=$1 AND organization_id=$2 AND country_code=$3 AND version=$4 FOR UPDATE").bind(&s.registry_id).bind(&s.organization_id).bind(country).bind(expected).fetch_optional(&mut *tx).await.map_err(err)?.ok_or(ProfileError::NotFound)?;
        let state: String = row.try_get("state").map_err(err)?;
        if state != "draft" {
            return Err(ProfileError::Conflict);
        }
        sqlx::query("UPDATE litecr.country_profile_version SET state='published',updated_by=$5,updated_at=now() WHERE registry_id=$1 AND organization_id=$2 AND country_code=$3 AND version=$4").bind(&s.registry_id).bind(&s.organization_id).bind(country).bind(expected).bind(a).execute(&mut *tx).await.map_err(err)?;
        sqlx::query("INSERT INTO litecr.country_profile_active(registry_id,organization_id,country_code,active_version) VALUES($1,$2,$3,$4) ON CONFLICT(registry_id,organization_id,country_code) DO UPDATE SET active_version=EXCLUDED.active_version").bind(&s.registry_id).bind(&s.organization_id).bind(country).bind(expected).execute(&mut *tx).await.map_err(err)?;
        sqlx::query("INSERT INTO litecr.configuration_audit(registry_id,organization_id,actor_ref,action,resource_key,version) VALUES($1,$2,$3,'country-profile.publish',$4,$5)").bind(&s.registry_id).bind(&s.organization_id).bind(a).bind(country).bind(expected).execute(&mut *tx).await.map_err(err)?;
        let config: Value = row.try_get("config").map_err(err)?;
        tx.commit().await.map_err(err)?;
        decode(country.into(), expected, config)
    }
    async fn rollback(
        &self,
        s: &ResourceScope,
        country: &str,
        target: i64,
        expected: i64,
        a: &str,
    ) -> Result<CountryProfile, ProfileError> {
        let mut tx = self.pool.begin().await.map_err(err)?;
        let active: Option<i64> = sqlx::query_scalar(
            "SELECT active_version FROM litecr.country_profile_active WHERE registry_id=$1 AND organization_id=$2 AND country_code=$3 FOR UPDATE",
        )
        .bind(&s.registry_id)
        .bind(&s.organization_id)
        .bind(country)
        .fetch_optional(&mut *tx)
        .await
        .map_err(err)?;
        if active != Some(expected) {
            return Err(ProfileError::Conflict);
        }
        let config: Value = sqlx::query_scalar(
            "SELECT config FROM litecr.country_profile_version WHERE registry_id=$1 AND organization_id=$2 AND country_code=$3 AND version=$4 AND state='published'",
        )
        .bind(&s.registry_id)
        .bind(&s.organization_id)
        .bind(country)
        .bind(target)
        .fetch_optional(&mut *tx)
        .await
        .map_err(err)?
        .ok_or(ProfileError::NotFound)?;
        let version: i64 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(version),0)+1 FROM litecr.country_profile_version WHERE registry_id=$1 AND organization_id=$2 AND country_code=$3",
        )
        .bind(&s.registry_id)
        .bind(&s.organization_id)
        .bind(country)
        .fetch_one(&mut *tx)
        .await
        .map_err(err)?;
        sqlx::query(
            "INSERT INTO litecr.country_profile_version(registry_id,organization_id,country_code,version,config,state,updated_by) VALUES($1,$2,$3,$4,$5,'published',$6)",
        )
        .bind(&s.registry_id)
        .bind(&s.organization_id)
        .bind(country)
        .bind(version)
        .bind(&config)
        .bind(a)
        .execute(&mut *tx)
        .await
        .map_err(err)?;
        sqlx::query(
            "UPDATE litecr.country_profile_active SET active_version=$4 WHERE registry_id=$1 AND organization_id=$2 AND country_code=$3",
        )
        .bind(&s.registry_id)
        .bind(&s.organization_id)
        .bind(country)
        .bind(version)
        .execute(&mut *tx)
        .await
        .map_err(err)?;
        sqlx::query(
            "INSERT INTO litecr.configuration_audit(registry_id,organization_id,actor_ref,action,resource_key,version) VALUES($1,$2,$3,'country-profile.rollback',$4,$5)",
        )
        .bind(&s.registry_id)
        .bind(&s.organization_id)
        .bind(a)
        .bind(country)
        .bind(version)
        .execute(&mut *tx)
        .await
        .map_err(err)?;
        tx.commit().await.map_err(err)?;
        decode(country.into(), version, config)
    }
    async fn current(
        &self,
        s: &ResourceScope,
        country: &str,
    ) -> Result<Option<CountryProfile>, ProfileError> {
        let row=sqlx::query("SELECT v.config,v.version FROM litecr.country_profile_active a JOIN litecr.country_profile_version v ON v.registry_id=a.registry_id AND v.organization_id=a.organization_id AND v.country_code=a.country_code AND v.version=a.active_version WHERE a.registry_id=$1 AND a.organization_id=$2 AND a.country_code=$3").bind(&s.registry_id).bind(&s.organization_id).bind(country).fetch_optional(&self.pool).await.map_err(err)?;
        row.map(|r| {
            decode(
                country.to_owned(),
                r.try_get("version").map_err(err)?,
                r.try_get("config").map_err(err)?,
            )
        })
        .transpose()
    }
}
