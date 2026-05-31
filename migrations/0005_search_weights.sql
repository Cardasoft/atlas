-- Atlas DAM — migration 0005 : pondérations RRF par tenant (doc 25 §4.4 / §9)
-- Poids de la fusion hybride configurables par tenant : sémantique, lexical, popularité.
-- Un seul enregistrement par tenant (tenant_id = clé primaire) → upsert déterministe. RLS.
-- Défauts neutres : semantic=1, lexical=1, popularity=0 (pas de boost tant que non activé).
-- Idempotente (IF NOT EXISTS / garde sur la policy).

CREATE TABLE IF NOT EXISTS search_weights (
    tenant_id  uuid PRIMARY KEY REFERENCES tenant(id) ON DELETE CASCADE,
    semantic   real NOT NULL DEFAULT 1.0,
    lexical    real NOT NULL DEFAULT 1.0,
    popularity real NOT NULL DEFAULT 0.0,
    updated_at timestamptz NOT NULL DEFAULT now()
);

-- RLS : isolation par tenant (atlas.tenant positionné par transaction).
ALTER TABLE search_weights ENABLE ROW LEVEL SECURITY;
ALTER TABLE search_weights FORCE ROW LEVEL SECURITY;

DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_policies WHERE policyname = 'search_weights_tenant') THEN
        CREATE POLICY search_weights_tenant ON search_weights
            USING (tenant_id = current_setting('atlas.tenant', true)::uuid);
    END IF;
END$$;
