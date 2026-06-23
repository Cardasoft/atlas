-- Atlas DAM — migration 0006 : provenance & transparence IA (AI Act art. 50 / C2PA)
--
-- Le règlement européen AI Act (art. 50) impose, dès le 2 août 2026, le marquage et
-- l'étiquetage des contenus générés/manipulés par IA. On enregistre donc, par asset :
--   - ai_provenance : origine IA (human | ai_generated | ai_edited | unknown) ;
--   - c2pa_present  : présence d'un manifeste Content Credentials (C2PA) signé ;
--   - generator     : outil/modèle générateur déclaré, si connu.
-- Idempotente (IF NOT EXISTS / DROP+ADD du CHECK).

ALTER TABLE asset ADD COLUMN IF NOT EXISTS ai_provenance text NOT NULL DEFAULT 'unknown';
ALTER TABLE asset ADD COLUMN IF NOT EXISTS c2pa_present  boolean NOT NULL DEFAULT false;
ALTER TABLE asset ADD COLUMN IF NOT EXISTS generator     text;

-- Domaine fermé des valeurs d'origine IA (cohérent avec atlas_types::AiProvenance).
ALTER TABLE asset DROP CONSTRAINT IF EXISTS asset_ai_provenance_chk;
ALTER TABLE asset ADD CONSTRAINT asset_ai_provenance_chk
    CHECK (ai_provenance IN ('human', 'ai_generated', 'ai_edited', 'unknown'));

-- Index de filtrage/facette : retrouver vite les contenus à étiqueter (art. 50).
CREATE INDEX IF NOT EXISTS asset_provenance ON asset (tenant_id, ai_provenance);
