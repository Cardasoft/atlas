//! Persistance d'ingestion (doc 26 §9) — écritures minimales pour rendre la recherche
//! testable de bout en bout : tenant, asset, `search_text` (FTS), `embedding` (pgvector).
//!
//! TDD : `compose_search_text` est une fonction **pure** testée sans base. Les écritures
//! et la recherche sont validées par des tests d'**intégration `#[ignore]`** (base de test).

use crate::{Db, DbError};
use sqlx::Row;
use uuid::Uuid;

/// Concatène les sources textuelles (titre, caption, OCR, transcription) en un texte
/// unique destiné au `tsvector` (doc 25). Ignore les parties vides, sépare par espace.
pub fn compose_search_text(parts: &[&str]) -> String {
    parts
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

impl Db {
    /// Crée un tenant (bootstrap/tests) et renvoie son id.
    pub async fn create_tenant(&self, name: &str) -> Result<Uuid, DbError> {
        let row = sqlx::query("INSERT INTO tenant (name) VALUES ($1) RETURNING id")
            .bind(name)
            .fetch_one(&self.pool)
            .await?;
        Ok(row.get::<Uuid, _>("id"))
    }

    /// Insère un asset minimal et renvoie son id (contexte tenant positionné pour la RLS).
    pub async fn insert_asset(
        &self,
        tenant: Uuid,
        title: &str,
        mime: &str,
        status: &str,
        rights_status: &str,
        orientation: Option<&str>,
        has_people: Option<bool>,
    ) -> Result<Uuid, DbError> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("SELECT set_config('atlas.tenant', $1, true)")
            .bind(tenant.to_string())
            .execute(&mut *tx)
            .await?;
        let row = sqlx::query(
            r#"INSERT INTO asset (tenant_id, title, mime, status, rights_status, orientation, has_people)
               VALUES ($1,$2,$3,$4,$5,$6,$7) RETURNING id"#,
        )
        .bind(tenant)
        .bind(title)
        .bind(mime)
        .bind(status)
        .bind(rights_status)
        .bind(orientation)
        .bind(has_people)
        .fetch_one(&mut *tx)
        .await?;
        let id = row.get::<Uuid, _>("id");
        tx.commit().await?;
        Ok(id)
    }

    /// Indexe le texte d'un asset (FTS). `tsv` calculé par PostgreSQL.
    /// Contexte tenant positionné : la RLS (USING = WITH CHECK) autorise l'INSERT.
    pub async fn upsert_search_text(
        &self,
        tenant: Uuid,
        asset_id: Uuid,
        lang: &str,
        text: &str,
    ) -> Result<(), DbError> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("SELECT set_config('atlas.tenant', $1, true)")
            .bind(tenant.to_string())
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            r#"INSERT INTO search_text (asset_id, tenant_id, lang, tsv)
               VALUES ($1,$2,$3, to_tsvector($3::regconfig, $4))
               ON CONFLICT (asset_id) DO UPDATE SET tsv = EXCLUDED.tsv, lang = EXCLUDED.lang"#,
        )
        .bind(asset_id)
        .bind(tenant)
        .bind(lang)
        .bind(text)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Écrit l'embedding multimodal d'un asset (pgvector).
    pub async fn upsert_embedding(
        &self,
        tenant: Uuid,
        asset_id: Uuid,
        model: &str,
        vec: &[f32],
    ) -> Result<(), DbError> {
        let lit = crate::vector::pgvector_literal(vec);
        let mut tx = self.pool.begin().await?;
        sqlx::query("SELECT set_config('atlas.tenant', $1, true)")
            .bind(tenant.to_string())
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            r#"INSERT INTO embedding (asset_id, tenant_id, kind, model, dim, vec)
               VALUES ($1,$2,'multimodal',$3,$4,$5::vector)
               ON CONFLICT (asset_id, kind) DO UPDATE SET vec = EXCLUDED.vec, model = EXCLUDED.model"#,
        )
        .bind(asset_id)
        .bind(tenant)
        .bind(model)
        .bind(vec.len() as i32)
        .bind(lit)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compose_joins_non_empty_parts() {
        assert_eq!(
            compose_search_text(&["Plage été", "", "  ", "coucher de soleil"]),
            "Plage été coucher de soleil"
        );
    }

    #[test]
    fn compose_empty_when_all_blank() {
        assert_eq!(compose_search_text(&["", "   "]), "");
    }

    // --- Intégration (nécessite Postgres+pgvector) : `ATLAS_TEST_DATABASE_URL=... cargo test -- --ignored`
    // Prouve la chaîne complète : insert asset + search_text + embedding → FTS + kNN, sous RLS.
    #[tokio::test]
    #[ignore = "nécessite une base de test (ATLAS_TEST_DATABASE_URL)"]
    async fn end_to_end_lexical_and_vector() {
        use atlas_embed::{Embedder, FakeEmbedder};

        let url = std::env::var("ATLAS_TEST_DATABASE_URL").expect("ATLAS_TEST_DATABASE_URL");
        let db = Db::connect(&url).await.unwrap();
        let tenant = db.create_tenant("test").await.unwrap();

        let a = db
            .insert_asset(tenant, "Plage au coucher de soleil", "image/jpeg", "READY", "valid", Some("landscape"), Some(false))
            .await
            .unwrap();
        db.upsert_search_text(tenant, a, "french", "plage coucher de soleil mer").await.unwrap();

        let emb = FakeEmbedder;
        db.upsert_embedding(tenant, a, "fake", &emb.encode("plage")).await.unwrap();

        // Lexical : la requête « plage » doit retrouver l'asset.
        let lex = db.lexical_search(tenant, "plage", "french", 10).await.unwrap();
        assert!(lex.contains(&a), "FTS doit retrouver l'asset par 'plage'");

        // Vectoriel : kNN retourne l'asset (un seul embedding en base).
        let filter = atlas_search::understanding::StructuredFilter::default();
        let vec = db.vector_search(tenant, &emb.encode("plage"), &filter, 10).await.unwrap();
        assert!(vec.contains(&a), "kNN doit retrouver l'asset");

        // Hydratation : asset_summaries renvoie titre + droits pour l'asset visible (doc 25 §5).
        let sums = db.asset_summaries(tenant, &[a]).await.unwrap();
        let (id, title, rights) = sums.iter().find(|(id, _, _)| *id == a).expect("résumé présent");
        assert_eq!(*id, a);
        assert_eq!(title.as_deref(), Some("Plage au coucher de soleil"));
        assert_eq!(rights, "valid");

        // Facettes : l'asset « landscape » doit apparaître dans la facette orientation (doc 25 §4.5).
        let facets = db.facet_counts(tenant, 20).await.unwrap();
        let (_, orient) = facets.iter().find(|(name, _)| name == "orientation").expect("facette orientation");
        assert!(orient.iter().any(|(v, c)| v == "landscape" && *c >= 1));

        // facet_config : restreindre aux seules facettes configurées (doc 25 §4.5).
        db.put_facet_config(tenant, "tenant", r#"["mime"]"#).await.unwrap();
        let fields = db.facet_config_fields(tenant, "tenant").await.unwrap();
        assert_eq!(fields, vec!["mime".to_string()]);
    }

    #[tokio::test]
    #[ignore = "nécessite une base de test (ATLAS_TEST_DATABASE_URL)"]
    async fn rls_isolates_tenants() {
        let url = std::env::var("ATLAS_TEST_DATABASE_URL").expect("ATLAS_TEST_DATABASE_URL");
        let db = Db::connect(&url).await.unwrap();
        let t1 = db.create_tenant("t1").await.unwrap();
        let t2 = db.create_tenant("t2").await.unwrap();

        let a1 = db
            .insert_asset(t1, "secret t1", "image/jpeg", "READY", "valid", None, None)
            .await
            .unwrap();
        db.upsert_search_text(t1, a1, "simple", "secret").await.unwrap();

        // Recherche dans le contexte de t2 : ne doit JAMAIS voir l'asset de t1 (RLS).
        let res = db.lexical_search(t2, "secret", "simple", 10).await.unwrap();
        assert!(!res.contains(&a1), "fuite inter-tenant : RLS défaillante");
    }
}
