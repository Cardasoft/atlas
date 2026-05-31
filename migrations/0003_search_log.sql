-- Atlas DAM — migration 0003 : journal des recherches (doc 25 §3.2 / §6)
-- Alimente le calcul offline du nDCG@10 (golden set) et le signal de popularité.
-- `user_id` pseudonymisable ; `interpreted` = sortie d'understanding (jsonb). RLS par tenant.
-- Idempotente (IF NOT EXISTS / garde sur la policy).

CREATE TABLE IF NOT EXISTS search_log (
    id            bigserial PRIMARY KEY,
    tenant_id     uuid NOT NULL REFERENCES tenant(id) ON DELETE CASCADE,
    user_id       uuid,
    query_hash    text NOT NULL,
    interpreted   jsonb NOT NULL,
    result_count  int  NOT NULL,
    clicked       uuid[] NOT NULL DEFAULT '{}',
    latency_ms    int,
    degraded      boolean NOT NULL DEFAULT false,
    created_at    timestamptz NOT NULL DEFAULT now()
);
-- Analyse temporelle par tenant (popularité, nDCG offline).
CREATE INDEX IF NOT EXISTS search_log_tenant_time ON search_log (tenant_id, created_at DESC);

-- RLS : isolation par tenant (atlas.tenant positionné par transaction).
ALTER TABLE search_log ENABLE ROW LEVEL SECURITY;
ALTER TABLE search_log FORCE ROW LEVEL SECURITY;

DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_policies WHERE policyname = 'search_log_tenant') THEN
        CREATE POLICY search_log_tenant ON search_log
            USING (tenant_id = current_setting('atlas.tenant', true)::uuid);
    END IF;
END$$;
