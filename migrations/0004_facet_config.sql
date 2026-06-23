-- Atlas DAM — migration 0004 : configuration des facettes (doc 25 §3.2/§4.5)
-- Liste ordonnée de facettes (champs) par périmètre (tenant / rôle / espace / portail).
-- Pilote quelles facettes la recherche calcule. UNIQUE (tenant, scope) pour l'upsert. RLS.
-- Idempotente (IF NOT EXISTS / garde sur la policy).

CREATE TABLE IF NOT EXISTS facet_config (
    id         uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id  uuid NOT NULL REFERENCES tenant(id) ON DELETE CASCADE,
    scope      text NOT NULL DEFAULT 'tenant',   -- 'tenant' | 'role:<r>' | 'space:<id>' | 'portal:<id>'
    facets     jsonb NOT NULL,                   -- liste ordonnée de champs (+ custom)
    updated_at timestamptz NOT NULL DEFAULT now()
);
-- Un seul enregistrement par (tenant, scope) → upsert déterministe.
CREATE UNIQUE INDEX IF NOT EXISTS facet_config_scope ON facet_config (tenant_id, scope);

-- RLS : isolation par tenant (atlas.tenant positionné par transaction).
ALTER TABLE facet_config ENABLE ROW LEVEL SECURITY;
ALTER TABLE facet_config FORCE ROW LEVEL SECURITY;

DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_policies WHERE policyname = 'facet_config_tenant') THEN
        CREATE POLICY facet_config_tenant ON facet_config
            USING (tenant_id = current_setting('atlas.tenant', true)::uuid);
    END IF;
END$$;
