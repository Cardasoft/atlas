//! Configuration des facettes par périmètre (doc 25 §3.2/§4.5). CRUD borné par la RLS.
//!
//! `facets` est un tableau JSON de noms de champs, stocké en `jsonb`. On l'écrit via un
//! cast `$N::jsonb` (bind texte) et on le relit soit en `::text` (restitution API), soit
//! déplié en lignes via `jsonb_array_elements_text` (liste de champs pour le calcul). Cela
//! évite la feature `json` de sqlx **et** un parseur JSON côté Rust (build hermétique).

use crate::{Db, DbError};
use sqlx::Row;
use uuid::Uuid;

impl Db {
    /// Upsert de la configuration des facettes d'un périmètre (tenant positionné, RLS).
    /// `facets_json` doit être un tableau JSON de chaînes (validé en `jsonb` par PostgreSQL).
    pub async fn put_facet_config(
        &self,
        tenant: Uuid,
        scope: &str,
        facets_json: &str,
    ) -> Result<(), DbError> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("SELECT set_config('atlas.tenant', $1, true)")
            .bind(tenant.to_string())
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            r#"INSERT INTO facet_config (tenant_id, scope, facets)
               VALUES ($1,$2,$3::jsonb)
               ON CONFLICT (tenant_id, scope)
               DO UPDATE SET facets = EXCLUDED.facets, updated_at = now()"#,
        )
        .bind(tenant)
        .bind(scope)
        .bind(facets_json)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Renvoie la configuration brute (`facets` en texte JSON) du périmètre, ou `None`.
    pub async fn get_facet_config(
        &self,
        tenant: Uuid,
        scope: &str,
    ) -> Result<Option<String>, DbError> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("SELECT set_config('atlas.tenant', $1, true)")
            .bind(tenant.to_string())
            .execute(&mut *tx)
            .await?;
        let row = sqlx::query("SELECT facets::text AS facets FROM facet_config WHERE scope = $1")
            .bind(scope)
            .fetch_optional(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(row.map(|r| r.get::<String, _>("facets")))
    }

    /// Liste ordonnée des champs de facette configurés pour le périmètre (vide si non configuré).
    /// Déplie le tableau `jsonb` côté PostgreSQL → pas de parseur JSON en Rust.
    pub async fn facet_config_fields(
        &self,
        tenant: Uuid,
        scope: &str,
    ) -> Result<Vec<String>, DbError> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("SELECT set_config('atlas.tenant', $1, true)")
            .bind(tenant.to_string())
            .execute(&mut *tx)
            .await?;
        let rows = sqlx::query(
            r#"SELECT jsonb_array_elements_text(facets) AS f
               FROM facet_config
               WHERE scope = $1 AND jsonb_typeof(facets) = 'array'"#,
        )
        .bind(scope)
        .fetch_all(&mut *tx)
        .await?;
        let out = rows.iter().map(|r| r.get::<String, _>("f")).collect();
        tx.commit().await?;
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Intégration (nécessite Postgres) : upsert → get/fields, et isolation inter-tenant.
    #[tokio::test]
    #[ignore = "nécessite une base de test (ATLAS_TEST_DATABASE_URL)"]
    async fn facet_config_upsert_and_isolation() {
        let url = std::env::var("ATLAS_TEST_DATABASE_URL").expect("ATLAS_TEST_DATABASE_URL");
        let db = Db::connect(&url).await.unwrap();
        let t1 = db.create_tenant("fc-t1").await.unwrap();
        let t2 = db.create_tenant("fc-t2").await.unwrap();

        db.put_facet_config(t1, "tenant", r#"["mime","orientation"]"#)
            .await
            .unwrap();
        let fields = db.facet_config_fields(t1, "tenant").await.unwrap();
        assert_eq!(fields, vec!["mime".to_string(), "orientation".to_string()]);

        // Upsert : remplace la liste.
        db.put_facet_config(t1, "tenant", r#"["rights_status"]"#)
            .await
            .unwrap();
        assert_eq!(
            db.facet_config_fields(t1, "tenant").await.unwrap(),
            vec!["rights_status".to_string()]
        );

        // Isolation : t2 n'a aucune config (RLS).
        assert!(db.get_facet_config(t2, "tenant").await.unwrap().is_none());
        assert!(db
            .facet_config_fields(t2, "tenant")
            .await
            .unwrap()
            .is_empty());
    }
}
