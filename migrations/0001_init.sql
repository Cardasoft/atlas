-- Atlas DAM — migration 0001 (M0 fondations)
-- Multi-tenant + RLS (doc 02 §8, doc 38). pgvector activé pour la recherche (doc 25).
-- Idempotente autant que possible (IF NOT EXISTS).

CREATE EXTENSION IF NOT EXISTS "pgcrypto";   -- gen_random_uuid()
CREATE EXTENSION IF NOT EXISTS vector;        -- pgvector (recherche)

-- Tenant : unité d'isolation (doc 16/38)
CREATE TABLE IF NOT EXISTS tenant (
    id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    name        text NOT NULL,
    config      jsonb NOT NULL DEFAULT '{}',
    quotas      jsonb NOT NULL DEFAULT '{}',
    ai_policy   jsonb NOT NULL DEFAULT '{"default":"local"}',  -- local par défaut (souverain)
    created_at  timestamptz NOT NULL DEFAULT now()
);

-- Asset : objet géré (colonnes utiles M0 ; s'enrichira, docs 05/25)
CREATE TABLE IF NOT EXISTS asset (
    id             uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id      uuid NOT NULL REFERENCES tenant(id) ON DELETE CASCADE,
    title          text,
    mime           text,
    mime_real      text,
    size           bigint,
    content_sha256 bytea,
    phash          bigint,
    orientation    text,
    has_people     boolean,
    status         text NOT NULL DEFAULT 'INGESTING',
    rights_status  text NOT NULL DEFAULT 'none',
    captured_at    timestamptz,
    created_at     timestamptz NOT NULL DEFAULT now(),
    updated_at     timestamptz NOT NULL DEFAULT now(),
    created_by     uuid
);
CREATE UNIQUE INDEX IF NOT EXISTS asset_sha_uniq ON asset (tenant_id, content_sha256)
    WHERE content_sha256 IS NOT NULL;
CREATE INDEX IF NOT EXISTS asset_filter ON asset (tenant_id, status, rights_status, orientation, captured_at);

-- Embedding multimodal (doc 25). Dimension SigLIP so400m = 1152 (à ajuster au modèle retenu).
CREATE TABLE IF NOT EXISTS embedding (
    asset_id  uuid NOT NULL REFERENCES asset(id) ON DELETE CASCADE,
    tenant_id uuid NOT NULL,
    kind      text NOT NULL DEFAULT 'multimodal',
    model     text NOT NULL,
    dim       int  NOT NULL,
    vec       vector(1152) NOT NULL,
    PRIMARY KEY (asset_id, kind)
);
-- Index HNSW (cosinus). ef_search réglé à la requête (doc 25 §3.3).
CREATE INDEX IF NOT EXISTS embedding_hnsw ON embedding
    USING hnsw (vec vector_cosine_ops) WITH (m = 16, ef_construction = 200);

-- Texte lexical (FTS) : titre + caption + ocr + transcription (doc 25)
CREATE TABLE IF NOT EXISTS search_text (
    asset_id  uuid PRIMARY KEY REFERENCES asset(id) ON DELETE CASCADE,
    tenant_id uuid NOT NULL,
    lang      text,
    tsv       tsvector NOT NULL DEFAULT to_tsvector('simple', '')
);
CREATE INDEX IF NOT EXISTS search_text_tsv ON search_text USING gin (tsv);

-- Audit immuable chaîné (doc 27)
CREATE TABLE IF NOT EXISTS audit_event (
    id         bigserial PRIMARY KEY,
    tenant_id  uuid NOT NULL,
    actor      uuid,
    action     text NOT NULL,
    target     text NOT NULL,
    from_state text,
    to_state   text,
    context    jsonb,
    ts         timestamptz NOT NULL DEFAULT now(),
    prev_hash  bytea,
    hash       bytea NOT NULL
);
CREATE INDEX IF NOT EXISTS audit_target ON audit_event (tenant_id, target, ts);

-- Row-Level Security : isolation par tenant (positionner atlas.tenant par transaction).
-- FORCE : la RLS s'applique AUSSI au propriétaire des tables (sinon contournée en dev).
-- La policy USING sert également de WITH CHECK → l'INSERT exige le bon tenant.
ALTER TABLE asset       ENABLE ROW LEVEL SECURITY;
ALTER TABLE embedding   ENABLE ROW LEVEL SECURITY;
ALTER TABLE search_text ENABLE ROW LEVEL SECURITY;
ALTER TABLE audit_event ENABLE ROW LEVEL SECURITY;
ALTER TABLE asset       FORCE ROW LEVEL SECURITY;
ALTER TABLE embedding   FORCE ROW LEVEL SECURITY;
ALTER TABLE search_text FORCE ROW LEVEL SECURITY;
ALTER TABLE audit_event FORCE ROW LEVEL SECURITY;

DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_policies WHERE policyname = 'asset_tenant') THEN
        CREATE POLICY asset_tenant ON asset
            USING (tenant_id = current_setting('atlas.tenant', true)::uuid);
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_policies WHERE policyname = 'embedding_tenant') THEN
        CREATE POLICY embedding_tenant ON embedding
            USING (tenant_id = current_setting('atlas.tenant', true)::uuid);
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_policies WHERE policyname = 'search_text_tenant') THEN
        CREATE POLICY search_text_tenant ON search_text
            USING (tenant_id = current_setting('atlas.tenant', true)::uuid);
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_policies WHERE policyname = 'audit_tenant') THEN
        CREATE POLICY audit_tenant ON audit_event
            USING (tenant_id = current_setting('atlas.tenant', true)::uuid);
    END IF;
END$$;
