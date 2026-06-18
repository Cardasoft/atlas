//! Pondérations RRF par tenant (doc 25 §4.4/§9). CRUD borné par la RLS.
//!
//! Un enregistrement par tenant (clé primaire `tenant_id`) ; `get` renvoie `None` si le
//! tenant n'a rien configuré → le pipeline retombe sur les défauts neutres. Les colonnes
//! sont des `real` (float4) : sqlx les lie nativement, sans feature supplémentaire.

use crate::{Db, DbError};
use sqlx::Row;
use uuid::Uuid;

impl Db {
    /// Upsert des poids RRF du tenant (RLS). Remplace l'enregistrement existant.
    pub async fn put_search_weights(
        &self,
        tenant: Uuid,
        semantic: f32,
        lexical: f32,
        popularity: f32,
    ) -> Result<(), DbError> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("SELECT set_config('atlas.tenant', $1, true)")
            .bind(tenant.to_string())
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            r#"INSERT INTO search_weights (tenant_id, semantic, lexical, popularity)
               VALUES ($1,$2,$3,$4)
               ON CONFLICT (tenant_id)
               DO UPDATE SET semantic = EXCLUDED.semantic,
                             lexical = EXCLUDED.lexical,
                             popularity = EXCLUDED.popularity,
                             updated_at = now()"#,
        )
        .bind(tenant)
        .bind(semantic)
        .bind(lexical)
        .bind(popularity)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Renvoie les poids RRF `(semantic, lexical, popularity)` du tenant, ou `None` si non
    /// configuré (le pipeline appliquera alors les défauts).
    pub async fn get_search_weights(
        &self,
        tenant: Uuid,
    ) -> Result<Option<(f32, f32, f32)>, DbError> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("SELECT set_config('atlas.tenant', $1, true)")
            .bind(tenant.to_string())
            .execute(&mut *tx)
            .await?;
        let row = sqlx::query("SELECT semantic, lexical, popularity FROM search_weights")
            .fetch_optional(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(row.map(|r| {
            (
                r.get::<f32, _>("semantic"),
                r.get::<f32, _>("lexical"),
                r.get::<f32, _>("popularity"),
            )
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Intégration (nécessite Postgres) : upsert → get, et isolation inter-tenant.
    #[tokio::test]
    #[ignore = "nécessite une base de test (ATLAS_TEST_DATABASE_URL)"]
    async fn search_weights_upsert_and_isolation() {
        let url = std::env::var("ATLAS_TEST_DATABASE_URL").expect("ATLAS_TEST_DATABASE_URL");
        let db = Db::connect(&url).await.unwrap();
        let t1 = db.create_tenant("w-t1").await.unwrap();
        let t2 = db.create_tenant("w-t2").await.unwrap();

        // Non configuré → None (défauts appliqués en aval).
        assert!(db.get_search_weights(t1).await.unwrap().is_none());

        db.put_search_weights(t1, 2.0, 1.0, 0.5).await.unwrap();
        assert_eq!(
            db.get_search_weights(t1).await.unwrap(),
            Some((2.0, 1.0, 0.5))
        );

        // Upsert : remplace.
        db.put_search_weights(t1, 1.0, 3.0, 0.0).await.unwrap();
        assert_eq!(
            db.get_search_weights(t1).await.unwrap(),
            Some((1.0, 3.0, 0.0))
        );

        // Isolation : t2 reste sans config (RLS).
        assert!(db.get_search_weights(t2).await.unwrap().is_none());
    }
}
