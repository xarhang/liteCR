-- Runtime catalogs and presentation configuration contain no registry/patient data.
CREATE TABLE litecr.configuration_audit (
    audit_id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    registry_id TEXT NOT NULL,
    organization_id TEXT NOT NULL,
    actor_ref TEXT NOT NULL,
    action TEXT NOT NULL,
    resource_key TEXT NOT NULL,
    version BIGINT,
    occurred_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE litecr.catalog_locale (
    registry_id TEXT NOT NULL,
    organization_id TEXT NOT NULL,
    locale_tag TEXT NOT NULL CHECK (length(locale_tag) BETWEEN 2 AND 64),
    text_direction TEXT NOT NULL CHECK (text_direction IN ('ltr','rtl')),
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (registry_id, organization_id, locale_tag)
);
CREATE TABLE litecr.catalog_source (
    registry_id TEXT NOT NULL,
    organization_id TEXT NOT NULL,
    namespace TEXT NOT NULL CHECK (length(namespace) BETWEEN 1 AND 96),
    message_key TEXT NOT NULL CHECK (length(message_key) BETWEEN 1 AND 192),
    source_text TEXT NOT NULL CHECK (length(source_text) BETWEEN 1 AND 8192),
    source_revision BIGINT NOT NULL CHECK (source_revision > 0),
    placeholders TEXT[] NOT NULL DEFAULT '{}',
    plural BOOLEAN NOT NULL DEFAULT FALSE,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (registry_id, organization_id, namespace, message_key)
);
CREATE TABLE litecr.catalog_translation_draft (
    registry_id TEXT NOT NULL,
    organization_id TEXT NOT NULL,
    locale_tag TEXT NOT NULL,
    namespace TEXT NOT NULL,
    message_key TEXT NOT NULL,
    translation_text TEXT NOT NULL CHECK (length(translation_text) BETWEEN 1 AND 8192),
    plural_forms JSONB CHECK (plural_forms IS NULL OR (jsonb_typeof(plural_forms) = 'object' AND plural_forms ? 'other')),
    source_revision BIGINT NOT NULL CHECK (source_revision > 0),
    state TEXT NOT NULL CHECK (state IN ('draft','in_review','approved')),
    edited_by TEXT NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (registry_id, organization_id, locale_tag, namespace, message_key),
    FOREIGN KEY (registry_id, organization_id, locale_tag) REFERENCES litecr.catalog_locale(registry_id, organization_id, locale_tag),
    FOREIGN KEY (registry_id, organization_id, namespace, message_key) REFERENCES litecr.catalog_source(registry_id, organization_id, namespace, message_key)
);
CREATE TABLE litecr.catalog_bundle_release (
    registry_id TEXT NOT NULL,
    organization_id TEXT NOT NULL,
    locale_tag TEXT NOT NULL,
    namespace TEXT NOT NULL,
    revision BIGINT NOT NULL CHECK (revision > 0),
    source_revisions JSONB NOT NULL,
    bundle JSONB NOT NULL CHECK (jsonb_typeof(bundle) = 'object'),
    published_by TEXT NOT NULL,
    published_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (registry_id, organization_id, locale_tag, namespace, revision)
);
CREATE TABLE litecr.catalog_bundle_active (
    registry_id TEXT NOT NULL,
    organization_id TEXT NOT NULL,
    locale_tag TEXT NOT NULL,
    namespace TEXT NOT NULL,
    active_revision BIGINT NOT NULL,
    PRIMARY KEY (registry_id, organization_id, locale_tag, namespace),
    FOREIGN KEY (registry_id, organization_id, locale_tag, namespace, active_revision) REFERENCES litecr.catalog_bundle_release(registry_id, organization_id, locale_tag, namespace, revision)
);

CREATE TABLE litecr.country_profile_version (
    registry_id TEXT NOT NULL,
    organization_id TEXT NOT NULL,
    country_code TEXT NOT NULL CHECK (country_code ~ '^[A-Z]{2}$'),
    version BIGINT NOT NULL CHECK (version > 0),
    config JSONB NOT NULL CHECK (jsonb_typeof(config) = 'object' AND NOT (config ? 'matching_policy_ref')),
    state TEXT NOT NULL CHECK (state IN ('draft','published')),
    updated_by TEXT NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (registry_id, organization_id, country_code, version)
);
CREATE TABLE litecr.country_profile_active (
    registry_id TEXT NOT NULL,
    organization_id TEXT NOT NULL,
    country_code TEXT NOT NULL,
    active_version BIGINT NOT NULL,
    PRIMARY KEY (registry_id, organization_id, country_code),
    FOREIGN KEY (registry_id, organization_id, country_code, active_version) REFERENCES litecr.country_profile_version(registry_id, organization_id, country_code, version)
);

CREATE TABLE litecr.appearance_version (
    registry_id TEXT NOT NULL,
    organization_id TEXT NOT NULL,
    version BIGINT NOT NULL CHECK (version > 0),
    config JSONB NOT NULL CHECK (jsonb_typeof(config) = 'object'),
    state TEXT NOT NULL CHECK (state IN ('draft','published')),
    updated_by TEXT NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (registry_id, organization_id, version)
);
CREATE TABLE litecr.appearance_active (
    registry_id TEXT NOT NULL,
    organization_id TEXT NOT NULL,
    active_version BIGINT NOT NULL,
    PRIMARY KEY (registry_id, organization_id),
    FOREIGN KEY (registry_id, organization_id, active_version) REFERENCES litecr.appearance_version(registry_id, organization_id, version)
);
CREATE TABLE litecr.appearance_preference (
    registry_id TEXT NOT NULL,
    organization_id TEXT NOT NULL,
    actor_ref TEXT NOT NULL,
    color_mode TEXT CHECK (color_mode IS NULL OR color_mode IN ('light','dark','system')),
    density TEXT CHECK (density IS NULL OR density IN ('comfortable','compact')),
    high_contrast BOOLEAN,
    locale_tag TEXT CHECK (locale_tag IS NULL OR length(locale_tag) BETWEEN 2 AND 64),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (registry_id, organization_id, actor_ref)
);

UPDATE litecr.schema_meta SET version = 5, applied_at = now() WHERE singleton = TRUE AND version = 4;

CREATE FUNCTION litecr.prevent_release_mutation() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'published configuration releases are immutable';
END;
$$;
CREATE TRIGGER catalog_bundle_release_immutable
    BEFORE UPDATE OR DELETE ON litecr.catalog_bundle_release
    FOR EACH ROW EXECUTE FUNCTION litecr.prevent_release_mutation();

CREATE FUNCTION litecr.freeze_published_config() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF OLD.state = 'published' AND (TG_OP = 'DELETE' OR NEW.config IS DISTINCT FROM OLD.config OR NEW.state IS DISTINCT FROM OLD.state) THEN
        RAISE EXCEPTION 'published configuration versions are immutable';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER country_profile_version_immutable
    BEFORE UPDATE OR DELETE ON litecr.country_profile_version
    FOR EACH ROW EXECUTE FUNCTION litecr.freeze_published_config();
CREATE TRIGGER appearance_version_immutable
    BEFORE UPDATE OR DELETE ON litecr.appearance_version
    FOR EACH ROW EXECUTE FUNCTION litecr.freeze_published_config();

CREATE FUNCTION litecr.prevent_configuration_audit_mutation() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'configuration audit events are append only';
END;
$$;
CREATE TRIGGER configuration_audit_append_only
    BEFORE UPDATE OR DELETE ON litecr.configuration_audit
    FOR EACH ROW EXECUTE FUNCTION litecr.prevent_configuration_audit_mutation();
