-- Atlas DAM — migration 0002 : recherches enregistrées (doc 25 §3.2)
-- Une recherche enregistrée appartient à un tenant + un propriétaire ; le payload de
-- requête est stocké tel quel (jsonb) pour rejeu/édition. RLS par tenant comme le reste.
-- Idempotente (IF NOT EXISTS / garde sur la policy).

CREATE TABLE IF NOT EXISTS saved_search (
    id         uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id  uuid NOT NULL REFERENCES tenant(id) ON DELETE CASCADE,
    owner      uuid NOT NULL,
    name       text NOT NULL,
    query      jsonb NOT NULL,
    notify     boolean NOT NULL DEFAULT false,
    created_at timestamptz NOT NULL DEFAULT now()
);
-- Listing par propriétaire dans le tenant (ordre récent → ancien).
CREATE INDEX IF NOT EXISTS saved_search_owner ON saved_search (tenant_id, owner, created_at DESC);

-- RLS : isolation par tenant (atlas.tenant positionné par transaction).
-- La policy USING sert aussi de WITH CHECK → l'INSERT exige le bon tenant.
ALTER TABLE saved_search ENABLE ROW LEVEL SECURITY;
ALTER TABLE saved_search FORCE ROW LEVEL SECURITY;

DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_policies WHERE policyname = 'saved_search_tenant') THEN
        CREATE POLICY saved_search_tenant ON saved_search
            USING (tenant_id = current_setting('atlas.tenant', true)::uuid);
    END IF;
END$$;
