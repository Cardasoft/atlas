//! Persistance d'ingestion (doc 26 §9) — écritures minimales pour rendre la recherche
//! testable de bout en bout : tenant, asset, `search_text` (FTS), `embedding` (pgvector).
//!
//! TDD : `compose_search_text` est une fonction **pure** testée sans base. Les écritures
//! et la recherche sont validées par des tests d'**intégration `#[ignore]`** (base de test).

use crate::{Db, DbError};
use sqlx::Row;
use uuid::Uuid;

/// Vue complète d'un asset pour `GET /v1/assets/{id}` (AT-006). Mappe le schéma OpenAPI `Asset`
/// (`id`, `tenant_id`, `title`, `mime`, `status`, `rights_status`, `provenance`).
#[derive(Debug, Clone)]
pub struct AssetRecord {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub title: Option<String>,
    pub mime: Option<String>,
    pub status: String,
    pub rights_status: String,
    pub provenance: atlas_types::Provenance,
}

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
    /// `provenance` porte la transparence IA (AI Act art. 50) : origine, C2PA, générateur.
    #[allow(clippy::too_many_arguments)]
    pub async fn insert_asset(
        &self,
        tenant: Uuid,
        title: &str,
        mime: &str,
        status: &str,
        rights_status: &str,
        orientation: Option<&str>,
        has_people: Option<bool>,
        provenance: &atlas_types::Provenance,
    ) -> Result<Uuid, DbError> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("SELECT set_config('atlas.tenant', $1, true)")
            .bind(tenant.to_string())
            .execute(&mut *tx)
            .await?;
        let row = sqlx::query(
            r#"INSERT INTO asset
                 (tenant_id, title, mime, status, rights_status, orientation, has_people,
                  ai_provenance, c2pa_present, generator)
               VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10) RETURNING id"#,
        )
        .bind(tenant)
        .bind(title)
        .bind(mime)
        .bind(status)
        .bind(rights_status)
        .bind(orientation)
        .bind(has_people)
        .bind(provenance.ai.as_str())
        .bind(provenance.c2pa_present)
        .bind(provenance.generator.as_deref())
        .fetch_one(&mut *tx)
        .await?;
        let id = row.get::<Uuid, _>("id");
        tx.commit().await?;
        Ok(id)
    }

    /// Lit la provenance d'un asset (transparence IA, AI Act art. 50). `None` si absent.
    pub async fn asset_provenance(
        &self,
        tenant: Uuid,
        asset_id: Uuid,
    ) -> Result<Option<atlas_types::Provenance>, DbError> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("SELECT set_config('atlas.tenant', $1, true)")
            .bind(tenant.to_string())
            .execute(&mut *tx)
            .await?;
        let row =
            sqlx::query("SELECT ai_provenance, c2pa_present, generator FROM asset WHERE id = $1")
                .bind(asset_id)
                .fetch_optional(&mut *tx)
                .await?;
        tx.commit().await?;
        Ok(row.map(|r| atlas_types::Provenance {
            ai: atlas_types::AiProvenance::from_token(&r.get::<String, _>("ai_provenance")),
            c2pa_present: r.get::<bool, _>("c2pa_present"),
            generator: r.get::<Option<String>, _>("generator"),
        }))
    }

    /// Lit un asset complet par id, borné au tenant courant par la RLS `FORCE` (AT-006).
    /// `None` si l'asset n'existe pas **ou** appartient à un autre tenant : la RLS le rend
    /// invisible → l'API répond 404 sans fuiter l'existence d'un asset inter-tenant.
    pub async fn get_asset(
        &self,
        tenant: Uuid,
        asset_id: Uuid,
    ) -> Result<Option<AssetRecord>, DbError> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("SELECT set_config('atlas.tenant', $1, true)")
            .bind(tenant.to_string())
            .execute(&mut *tx)
            .await?;
        let row = sqlx::query(
            "SELECT id, tenant_id, title, mime, status, rights_status, \
             ai_provenance, c2pa_present, generator FROM asset WHERE id = $1",
        )
        .bind(asset_id)
        .fetch_optional(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(row.map(|r| AssetRecord {
            id: r.get::<Uuid, _>("id"),
            tenant_id: r.get::<Uuid, _>("tenant_id"),
            title: r.get::<Option<String>, _>("title"),
            mime: r.get::<Option<String>, _>("mime"),
            status: r.get::<String, _>("status"),
            rights_status: r.get::<String, _>("rights_status"),
            provenance: atlas_types::Provenance {
                ai: atlas_types::AiProvenance::from_token(&r.get::<String, _>("ai_provenance")),
                c2pa_present: r.get::<bool, _>("c2pa_present"),
                generator: r.get::<Option<String>, _>("generator"),
            },
        }))
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
            .insert_asset(
                tenant,
                "Plage au coucher de soleil",
                "image/jpeg",
                "READY",
                "valid",
                Some("landscape"),
                Some(false),
                &atlas_types::Provenance::default(),
            )
            .await
            .unwrap();
        db.upsert_search_text(tenant, a, "french", "plage coucher de soleil mer")
            .await
            .unwrap();

        let emb = FakeEmbedder;
        db.upsert_embedding(tenant, a, "fake", &emb.encode("plage"))
            .await
            .unwrap();

        // Lexical : la requête « plage » doit retrouver l'asset.
        let lex = db
            .lexical_search(tenant, "plage", "french", 10)
            .await
            .unwrap();
        assert!(lex.contains(&a), "FTS doit retrouver l'asset par 'plage'");

        // Vectoriel : kNN retourne l'asset (un seul embedding en base).
        let filter = atlas_search::understanding::StructuredFilter::default();
        let vec = db
            .vector_search(tenant, &emb.encode("plage"), &filter, 10)
            .await
            .unwrap();
        assert!(vec.contains(&a), "kNN doit retrouver l'asset");

        // Hydratation : asset_summaries renvoie titre + droits pour l'asset visible (doc 25 §5).
        let sums = db.asset_summaries(tenant, &[a]).await.unwrap();
        let (id, title, rights) = sums
            .iter()
            .find(|(id, _, _)| *id == a)
            .expect("résumé présent");
        assert_eq!(*id, a);
        assert_eq!(title.as_deref(), Some("Plage au coucher de soleil"));
        assert_eq!(rights, "valid");

        // Facettes : l'asset « landscape » doit apparaître dans la facette orientation (doc 25 §4.5).
        let facets = db.facet_counts(tenant, 20).await.unwrap();
        let (_, orient) = facets
            .iter()
            .find(|(name, _)| name == "orientation")
            .expect("facette orientation");
        assert!(orient.iter().any(|(v, c)| v == "landscape" && *c >= 1));

        // facet_config : restreindre aux seules facettes configurées (doc 25 §4.5).
        db.put_facet_config(tenant, "tenant", r#"["mime"]"#)
            .await
            .unwrap();
        let fields = db.facet_config_fields(tenant, "tenant").await.unwrap();
        assert_eq!(fields, vec!["mime".to_string()]);

        // Recherche par l'exemple (doc 25 §4.2) : un 2e asset embarqué doit être retrouvé
        // depuis l'embedding de `a`, et `a` lui-même exclu des résultats.
        let b = db
            .insert_asset(
                tenant,
                "Autre plage",
                "image/jpeg",
                "READY",
                "valid",
                Some("landscape"),
                Some(false),
                &atlas_types::Provenance::default(),
            )
            .await
            .unwrap();
        db.upsert_embedding(tenant, b, "fake", &emb.encode("plage"))
            .await
            .unwrap();
        let by_ex = db
            .vector_search_by_example(tenant, a, &filter, 10)
            .await
            .unwrap();
        assert!(by_ex.contains(&b), "par l'exemple doit retrouver le voisin");
        assert!(!by_ex.contains(&a), "la source doit être exclue");
    }

    #[tokio::test]
    #[ignore = "nécessite une base de test (ATLAS_TEST_DATABASE_URL)"]
    async fn rls_isolates_tenants() {
        let url = std::env::var("ATLAS_TEST_DATABASE_URL").expect("ATLAS_TEST_DATABASE_URL");
        let db = Db::connect(&url).await.unwrap();
        let t1 = db.create_tenant("t1").await.unwrap();
        let t2 = db.create_tenant("t2").await.unwrap();

        let a1 = db
            .insert_asset(
                t1,
                "secret t1",
                "image/jpeg",
                "READY",
                "valid",
                None,
                None,
                &atlas_types::Provenance::default(),
            )
            .await
            .unwrap();
        db.upsert_search_text(t1, a1, "simple", "secret")
            .await
            .unwrap();

        // Recherche dans le contexte de t2 : ne doit JAMAIS voir l'asset de t1 (RLS).
        let res = db.lexical_search(t2, "secret", "simple", 10).await.unwrap();
        assert!(!res.contains(&a1), "fuite inter-tenant : RLS défaillante");
    }

    // Provenance / transparence IA (AI Act art. 50) : persistance, relecture et facette.
    #[tokio::test]
    #[ignore = "nécessite une base de test (ATLAS_TEST_DATABASE_URL)"]
    async fn provenance_persisted_read_back_and_faceted() {
        use atlas_types::{AiProvenance, Provenance};

        let url = std::env::var("ATLAS_TEST_DATABASE_URL").expect("ATLAS_TEST_DATABASE_URL");
        let db = Db::connect(&url).await.unwrap();
        let tenant = db.create_tenant("prov").await.unwrap();

        let prov = Provenance {
            ai: AiProvenance::AiGenerated,
            c2pa_present: true,
            generator: Some("Firefly".into()),
        };
        let a = db
            .insert_asset(
                tenant,
                "Affiche IA",
                "image/png",
                "READY",
                "valid",
                None,
                None,
                &prov,
            )
            .await
            .unwrap();

        // Relecture fidèle de la provenance.
        let back = db
            .asset_provenance(tenant, a)
            .await
            .unwrap()
            .expect("présent");
        assert_eq!(back, prov);

        // Facette : la valeur « ai_generated » doit apparaître au moins une fois.
        let facets = db.facet_counts(tenant, 20).await.unwrap();
        let (_, vals) = facets
            .iter()
            .find(|(name, _)| name == "ai_provenance")
            .expect("facette ai_provenance");
        assert!(vals.iter().any(|(v, c)| v == "ai_generated" && *c >= 1));
    }

    // AT-006 : `get_asset` relit un asset complet pour son tenant, et la RLS le rend
    // invisible (`None`) depuis un autre tenant → l'API renverra 404 sans fuite d'existence.
    #[tokio::test]
    #[ignore = "nécessite une base de test (ATLAS_TEST_DATABASE_URL)"]
    async fn get_asset_reads_back_and_is_tenant_isolated() {
        use atlas_types::{AiProvenance, Provenance};

        let url = std::env::var("ATLAS_TEST_DATABASE_URL").expect("ATLAS_TEST_DATABASE_URL");
        let db = Db::connect(&url).await.unwrap();
        let t1 = db.create_tenant("get-t1").await.unwrap();
        let t2 = db.create_tenant("get-t2").await.unwrap();

        let prov = Provenance {
            ai: AiProvenance::AiEdited,
            c2pa_present: false,
            generator: Some("Photoshop".into()),
        };
        let a = db
            .insert_asset(
                t1,
                "Visuel campagne",
                "image/png",
                "READY",
                "valid",
                None,
                None,
                &prov,
            )
            .await
            .unwrap();

        // Lecture par le tenant propriétaire : tous les champs du schéma `Asset`.
        let got = db.get_asset(t1, a).await.unwrap().expect("présent pour t1");
        assert_eq!(got.id, a);
        assert_eq!(got.tenant_id, t1);
        assert_eq!(got.title.as_deref(), Some("Visuel campagne"));
        assert_eq!(got.mime.as_deref(), Some("image/png"));
        assert_eq!(got.status, "READY");
        assert_eq!(got.rights_status, "valid");
        assert_eq!(got.provenance, prov);

        // Lecture par un autre tenant : invisible (RLS) → `None` → 404 côté API.
        assert!(
            db.get_asset(t2, a).await.unwrap().is_none(),
            "fuite inter-tenant : get_asset doit être borné par la RLS"
        );

        // Id inexistant : `None` également.
        assert!(db.get_asset(t1, Uuid::new_v4()).await.unwrap().is_none());
    }
}
