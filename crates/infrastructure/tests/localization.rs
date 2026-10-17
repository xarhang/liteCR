use litecr_application::{
    access::ResourceScope,
    appearance::{AppearanceProfile, AppearanceRepository, ColorMode, Density, SemanticTokens},
    country_profiles::{CountryProfile, CountryProfileRepository, ProfileError},
    localization::{
        CatalogRepository, DraftTranslation, Locale, MessageSource, ReviewDecision,
        TranslationState,
    },
};
use litecr_infrastructure::{
    appearance::PostgresAppearanceRepository, connect,
    country_profiles::PostgresCountryProfileRepository, localization::PostgresCatalogRepository,
    migrate,
};
use sqlx::PgPool;
use std::collections::BTreeSet;

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
async fn postgres_catalog_profiles_and_appearance_publish_atomically_and_rollback() {
    let database_url = std::env::var("LITECR_TEST_DATABASE_URL")
        .expect("set LITECR_TEST_DATABASE_URL to the isolated synthetic-only PostgreSQL DB");
    let pool = connect(&database_url, 4).await.expect("test DB connects");
    migrate(&pool)
        .await
        .expect("all ordered migrations are applied");
    let suffix = suffix();
    let scope = ResourceScope {
        registry_id: format!("test-registry-{suffix}"),
        organization_id: format!("test-org-{suffix}"),
        record_id: None,
    };
    let scenario = tokio::spawn(run_scenario(pool.clone(), scope.clone(), suffix.clone())).await;
    cleanup_test_rows(&pool, &scope).await;
    scenario.expect("scenario task joined; exact-scope cleanup ran even after panic");
    pool.close().await;
}

async fn run_scenario(pool: PgPool, scope: ResourceScope, suffix: String) {
    let actor = format!("synthetic-reviewer-{suffix}");
    let catalogs = PostgresCatalogRepository::new(pool.clone());

    catalogs
        .add_locale(
            &scope,
            &Locale {
                tag: "en-US".into(),
                direction: "ltr".into(),
                enabled: true,
            },
            &actor,
        )
        .await
        .expect("locale is stored without a rebuild");
    // Simulate a pre-canonicalization catalog key. API calls use the BCP 47
    // equivalent while the stored key and its history remain unchanged.
    sqlx::query("INSERT INTO litecr.catalog_locale(registry_id,organization_id,locale_tag,text_direction) VALUES($1,$2,'pt-br','ltr')")
        .bind(&scope.registry_id).bind(&scope.organization_id).execute(&pool).await
        .expect("legacy case-variant locale is seeded in this synthetic scope");
    assert_eq!(
        catalogs
            .resolve_locale_tag(&scope, "pt-BR")
            .await
            .expect("equivalent tag resolves")
            .as_deref(),
        Some("pt-br")
    );
    catalogs
        .add_locale(
            &scope,
            &Locale {
                tag: "PT-br".into(),
                direction: "ltr".into(),
                enabled: true,
            },
            &actor,
        )
        .await
        .expect("equivalent registration reuses the existing legacy key");
    let equivalent_locale_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM litecr.catalog_locale WHERE registry_id=$1 AND organization_id=$2 AND lower(locale_tag)='pt-br'")
        .bind(&scope.registry_id).bind(&scope.organization_id).fetch_one(&pool).await
        .expect("equivalent locale count is queried");
    assert_eq!(equivalent_locale_count, 1);

    let source = MessageSource {
        namespace: "common".into(),
        key: "greeting".into(),
        source_text: "Hello {{name}}".into(),
        source_revision: 1,
        placeholders: ["name".to_owned()].into_iter().collect(),
        plural: false,
    };
    catalogs
        .set_source(&scope, &source, &actor)
        .await
        .expect("source revision created");
    catalogs
        .save_draft(
            &scope,
            &DraftTranslation {
                locale: "en-US".into(),
                namespace: "common".into(),
                key: "greeting".into(),
                value: "Sabaidee {{name}}".into(),
                plural_forms: None,
                source_revision: 1,
                state: TranslationState::Draft,
                stale: false,
            },
            &format!("synthetic-translator-{suffix}"),
        )
        .await
        .expect("synthetic translation draft is stored");
    catalogs
        .save_draft(
            &scope,
            &DraftTranslation {
                locale: "pt-br".into(),
                namespace: "common".into(),
                key: "greeting".into(),
                value: "Olá {{name}}".into(),
                plural_forms: None,
                source_revision: 1,
                state: TranslationState::Draft,
                stale: false,
            },
            &format!("synthetic-legacy-translator-{suffix}"),
        )
        .await
        .expect("legacy-key translation draft is seeded");
    let legacy_key = catalogs
        .resolve_locale_tag(&scope, "PT-br")
        .await
        .expect("canonical input resolves to the legacy database key")
        .expect("legacy locale exists");
    let legacy_draft = catalogs
        .load_draft(&scope, &legacy_key, "common", "greeting")
        .await
        .expect("legacy draft remains readable")
        .expect("legacy draft exists");
    assert_eq!(legacy_draft.locale, "pt-br");
    assert_eq!(legacy_draft.value, "Olá {{name}}");

    sqlx::query("INSERT INTO litecr.catalog_locale(registry_id,organization_id,locale_tag,text_direction) VALUES($1,$2,'PT-BR','ltr')")
        .bind(&scope.registry_id).bind(&scope.organization_id).execute(&pool).await
        .expect("ambiguous legacy locale is seeded to exercise safe conflict behavior");
    assert!(matches!(
        catalogs.resolve_locale_tag(&scope, "pt-BR").await,
        Err(litecr_application::localization::CatalogError::Conflict)
    ));
    assert!(matches!(
        catalogs
            .add_locale(
                &scope,
                &Locale {
                    tag: "pt-BR".into(),
                    direction: "ltr".into(),
                    enabled: true
                },
                &actor
            )
            .await,
        Err(litecr_application::localization::CatalogError::Conflict)
    ));

    let plural_source = MessageSource {
        namespace: "common".into(),
        key: "items".into(),
        source_text: "{{count}} item(s) for {{name}}".into(),
        source_revision: 1,
        placeholders: ["count".to_owned(), "name".to_owned()]
            .into_iter()
            .collect(),
        plural: true,
    };
    catalogs
        .set_source(&scope, &plural_source, &actor)
        .await
        .expect("plural source is stored");
    let translator = format!("synthetic-plural-translator-{suffix}");
    let plural_forms: std::collections::BTreeMap<String, String> =
        std::collections::BTreeMap::from([
            ("one".into(), "{{count}} item for {{name}}".into()),
            ("other".into(), "{{count}} items for {{name}}".into()),
        ]);
    catalogs
        .save_draft(
            &scope,
            &DraftTranslation {
                locale: "en-US".into(),
                namespace: "common".into(),
                key: "items".into(),
                value: plural_forms["other"].clone(),
                plural_forms: Some(plural_forms),
                source_revision: 1,
                state: TranslationState::Draft,
                stale: false,
            },
            &translator,
        )
        .await
        .expect("plural category translations are stored");
    catalogs
        .review(
            &scope,
            "en-US",
            "common",
            "greeting",
            ReviewDecision::Approve,
            &actor,
        )
        .await
        .expect("different synthetic reviewer approves draft");
    catalogs
        .review(
            &scope,
            "en-US",
            "common",
            "items",
            ReviewDecision::Approve,
            &actor,
        )
        .await
        .expect("plural draft is separately reviewed");
    let first = catalogs
        .publish(&scope, "en-US", "common", None, &actor)
        .await
        .expect("first immutable bundle release is active");
    assert_eq!(first.revision, 1);
    assert_eq!(
        first.entries.get("items_one").map(String::as_str),
        Some("{{count}} item for {{name}}")
    );
    assert_eq!(
        first.entries.get("items_other").map(String::as_str),
        Some("{{count}} items for {{name}}")
    );
    assert_eq!(
        catalogs
            .load_bundle(&scope, "en-US", "common")
            .await
            .unwrap()
            .unwrap(),
        first
    );
    assert!(
        catalogs
            .list_published_ui_locales(&scope)
            .await
            .expect("locale discovery query succeeds")
            .is_empty(),
        "publishing another namespace must not advertise a UI locale"
    );

    let shell_source = MessageSource {
        namespace: "ui".into(),
        key: "shell.brand".into(),
        source_text: "Identity registry".into(),
        source_revision: 1,
        placeholders: BTreeSet::new(),
        plural: false,
    };
    catalogs
        .set_source(&scope, &shell_source, &actor)
        .await
        .expect("UI source is stored");
    catalogs
        .save_draft(
            &scope,
            &DraftTranslation {
                locale: "en-US".into(),
                namespace: "ui".into(),
                key: "shell.brand".into(),
                value: "Approved synthetic UI label".into(),
                plural_forms: None,
                source_revision: 1,
                state: TranslationState::Draft,
                stale: false,
            },
            &format!("synthetic-ui-translator-{suffix}"),
        )
        .await
        .expect("UI translation draft is stored");
    catalogs
        .review(
            &scope,
            "en-US",
            "ui",
            "shell.brand",
            ReviewDecision::Approve,
            &actor,
        )
        .await
        .expect("reviewer approves UI translation");
    catalogs
        .publish(&scope, "en-US", "ui", None, &actor)
        .await
        .expect("UI catalog is published");
    let discoverable = catalogs
        .list_published_ui_locales(&scope)
        .await
        .expect("published locale discovery succeeds");
    assert_eq!(
        discoverable
            .iter()
            .map(|locale| locale.tag.as_str())
            .collect::<Vec<_>>(),
        vec!["en-US"]
    );

    let mut updated_source = source.clone();
    updated_source.source_text = "Good day {{name}}".into();
    updated_source.source_revision = 2;
    assert!(
        catalogs
            .set_source(&scope, &updated_source, &actor)
            .await
            .is_ok()
    );
    assert!(
        catalogs
            .publish(&scope, "en-US", "common", Some(1), &actor)
            .await
            .is_err_and(|error| matches!(
                error,
                litecr_application::localization::CatalogError::Conflict
            ))
    );
    let stale = catalogs
        .load_draft(&scope, "en-US", "common", "greeting")
        .await
        .unwrap()
        .unwrap();
    assert!(stale.stale, "source revision updates are surfaced as stale");

    let profile_repository = PostgresCountryProfileRepository::new(pool.clone());
    let country = CountryProfile {
        country_code: "LA".into(),
        default_locale: "lo-LA".into(),
        calendar: "gregory".into(),
        timezone: "Asia/Vientiane".into(),
        address_fields: vec!["province".into(), "district".into()],
        identifier_types: vec!["national-id".into()],
        phone_region: "LA".into(),
        matching_policy_ref: None,
        version: 1,
    };
    profile_repository
        .save_draft(&scope, &country, Some(0), &actor)
        .await
        .expect("bounded country draft stored");
    let first_country = profile_repository
        .publish(&scope, "LA", 1, &actor)
        .await
        .expect("country profile published");
    assert_eq!(first_country.country_code, "LA");
    assert_eq!(first_country.matching_policy_ref, None);
    assert_eq!(first_country.version, 1);
    let mut second_country = country.clone();
    second_country.default_locale = "en-LA".into();
    second_country.timezone = "Asia/Bangkok".into();
    second_country.version = 2;
    profile_repository
        .save_draft(&scope, &second_country, Some(1), &actor)
        .await
        .expect("updated country profile draft stored");
    let published_country = profile_repository
        .publish(&scope, "LA", 2, &actor)
        .await
        .expect("updated country profile published");
    assert_eq!(published_country.version, 2);
    assert_eq!(published_country.default_locale, "en-LA");
    let rolled_back_country = profile_repository
        .rollback(&scope, "LA", 1, 2, &actor)
        .await
        .expect("rollback creates a new immutable release");
    assert_eq!(rolled_back_country.version, 3);
    assert_eq!(rolled_back_country.default_locale, "lo-LA");
    assert_eq!(rolled_back_country.timezone, "Asia/Vientiane");
    assert!(
        profile_repository
            .rollback(&scope, "LA", 1, 2, &actor)
            .await
            .is_err_and(|error| matches!(error, ProfileError::Conflict))
    );
    assert!(
        profile_repository
            .rollback(&scope, "LA", 99, 3, &actor)
            .await
            .is_err_and(|error| matches!(error, ProfileError::NotFound))
    );
    let current_country = profile_repository
        .current(&scope, "LA")
        .await
        .expect("current country profile loads")
        .expect("rollback release is active");
    assert_eq!(current_country, rolled_back_country);
    let mut pending_country = country.clone();
    pending_country.version = 4;
    pending_country.default_locale = "en-LA".into();
    profile_repository
        .save_draft(&scope, &pending_country, Some(3), &actor)
        .await
        .expect("unpublished update is included in coverage inventory");
    let mut custom_country = country.clone();
    custom_country.country_code = "ZZ".into();
    custom_country.phone_region = "ZZ".into();
    profile_repository
        .save_draft(&scope, &custom_country, Some(0), &actor)
        .await
        .expect("custom profile code is included in coverage inventory");
    let coverage = profile_repository
        .coverage(&scope)
        .await
        .expect("coverage inventory loads for the current organization");
    assert_eq!(coverage.len(), 2);
    assert!(coverage.contains(
        &litecr_application::country_profiles::CountryProfileCoverage {
            country_code: "LA".into(),
            latest_version: 4,
            active_version: Some(3),
        }
    ));
    assert!(coverage.contains(
        &litecr_application::country_profiles::CountryProfileCoverage {
            country_code: "ZZ".into(),
            latest_version: 1,
            active_version: None,
        }
    ));
    let rollback_audit: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM litecr.configuration_audit WHERE registry_id=$1 AND organization_id=$2 AND action='country-profile.rollback' AND resource_key='LA' AND version=3",
    )
    .bind(&scope.registry_id)
    .bind(&scope.organization_id)
    .fetch_one(&pool)
    .await
    .expect("country rollback audit is queried");
    assert_eq!(rollback_audit, 1);

    let appearance_repository = PostgresAppearanceRepository::new(pool.clone());
    let appearance = AppearanceProfile {
        name: "Synthetic test brand".into(),
        accent: "#0F766E".into(),
        on_accent: "#FFFFFF".into(),
        logo_asset_ref: Some("test-logo".into()),
        color_mode: ColorMode::System,
        density: Density::Comfortable,
        high_contrast: false,
        tokens: SemanticTokens {
            canvas: "#F8FAFC".into(),
            surface: "#FFFFFF".into(),
            text: "#0F172A".into(),
            text_muted: "#475569".into(),
            action: "#0F766E".into(),
            on_action: "#FFFFFF".into(),
            control_border: "#64748B".into(),
            focus: "#1D4ED8".into(),
        },
        version: 1,
    };
    appearance_repository
        .save_draft(&scope, &appearance, Some(0), &actor)
        .await
        .expect("appearance draft stored");
    appearance_repository
        .publish(&scope, 0, &actor)
        .await
        .expect("initial appearance release published");
    let mut next_appearance = appearance.clone();
    next_appearance.name = "Changed synthetic brand".into();
    appearance_repository
        .save_draft(&scope, &next_appearance, Some(1), &actor)
        .await
        .expect("second appearance draft stored");
    appearance_repository
        .publish(&scope, 1, &actor)
        .await
        .expect("second appearance release published");
    let rolled_back = appearance_repository
        .rollback(&scope, 1, 2, &actor)
        .await
        .expect("rollback creates a new release without mutating the old one");
    assert_eq!(rolled_back.name, "Synthetic test brand");
    assert_eq!(rolled_back.version, 3);

    let audit_events: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) AS n FROM litecr.configuration_audit WHERE registry_id=$1 AND organization_id=$2",
    )
    .bind(&scope.registry_id)
    .bind(&scope.organization_id)
    .fetch_one(&pool)
    .await
    .expect("configuration audit is queried");
    assert!(
        audit_events >= 7,
        "each write/release action leaves audit evidence"
    );
}

async fn cleanup_test_rows(pool: &PgPool, scope: &ResourceScope) {
    let mut tx = pool.begin().await.expect("cleanup transaction begins");
    for (table, trigger) in [
        (
            "catalog_bundle_release",
            Some("catalog_bundle_release_immutable"),
        ),
        (
            "country_profile_version",
            Some("country_profile_version_immutable"),
        ),
        ("appearance_version", Some("appearance_version_immutable")),
        (
            "configuration_audit",
            Some("configuration_audit_append_only"),
        ),
    ] {
        let sql = format!(
            "ALTER TABLE litecr.{table} DISABLE TRIGGER {}",
            trigger.unwrap()
        );
        sqlx::query(&sql).execute(&mut *tx).await.expect("test DB owner can disable the exact P2-C immutability trigger within cleanup transaction");
    }
    for table in [
        "catalog_bundle_active",
        "catalog_bundle_release",
        "catalog_translation_draft",
        "catalog_source",
        "catalog_locale",
        "country_profile_active",
        "country_profile_version",
        "appearance_active",
        "appearance_preference",
        "appearance_version",
        "configuration_audit",
    ] {
        let sql = format!("DELETE FROM litecr.{table} WHERE registry_id=$1 AND organization_id=$2");
        sqlx::query(&sql)
            .bind(&scope.registry_id)
            .bind(&scope.organization_id)
            .execute(&mut *tx)
            .await
            .expect("only this run's exact synthetic scope is removed");
    }
    for table in [
        "catalog_bundle_active",
        "catalog_bundle_release",
        "catalog_translation_draft",
        "catalog_source",
        "catalog_locale",
        "country_profile_active",
        "country_profile_version",
        "appearance_active",
        "appearance_preference",
        "appearance_version",
        "configuration_audit",
    ] {
        let sql = format!(
            "SELECT COUNT(*) FROM litecr.{table} WHERE registry_id=$1 AND organization_id=$2"
        );
        let remaining: i64 = sqlx::query_scalar(&sql)
            .bind(&scope.registry_id)
            .bind(&scope.organization_id)
            .fetch_one(&mut *tx)
            .await
            .expect("cleanup postcondition query succeeds");
        assert_eq!(remaining, 0, "test-owned rows remain in {table}");
    }
    for (table, trigger) in [
        ("catalog_bundle_release", "catalog_bundle_release_immutable"),
        (
            "country_profile_version",
            "country_profile_version_immutable",
        ),
        ("appearance_version", "appearance_version_immutable"),
        ("configuration_audit", "configuration_audit_append_only"),
    ] {
        let sql = format!("ALTER TABLE litecr.{table} ENABLE TRIGGER {}", trigger);
        sqlx::query(&sql)
            .execute(&mut *tx)
            .await
            .expect("immutability trigger is restored before cleanup commits");
    }
    tx.commit()
        .await
        .expect("synthetic test rows and audit records cleaned");
}
