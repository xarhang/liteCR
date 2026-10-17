CREATE EXTENSION IF NOT EXISTS pg_trgm;

CREATE SCHEMA IF NOT EXISTS litecr;

CREATE TABLE litecr.schema_meta (
    singleton BOOLEAN PRIMARY KEY DEFAULT TRUE CHECK (singleton),
    version BIGINT NOT NULL,
    applied_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

INSERT INTO litecr.schema_meta (singleton, version)
VALUES (TRUE, 1)
ON CONFLICT (singleton) DO UPDATE
SET version = EXCLUDED.version,
    applied_at = now();

CREATE TABLE litecr.foundation_probe (
    probe_key TEXT PRIMARY KEY,
    probe_value TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX foundation_probe_value_trgm_idx
    ON litecr.foundation_probe USING GIN (probe_value gin_trgm_ops);
