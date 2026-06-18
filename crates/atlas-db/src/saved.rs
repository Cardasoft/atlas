//! Recherches enregistrées (doc 25 §3.2). CRUD borné par la RLS du tenant.
//!
//! Le payload `query` est stocké en `jsonb` mais transite ici comme **texte JSON** : on
//! l'insère via un cast `$N::jsonb` et on le relit en `query::text`. Cela évite d'activer
//! la feature `json` de sqlx (build hermétique, lockfile MSRV stable) tout en gardant la
//! validation jsonb côté PostgreSQL. La validité du JSON est la responsabilité de l'appelant.

use crate::{Db, DbError};
use sqlx::Row;
use uuid::Uuid;

/// Recherche enregistrée telle que restituée à l'API (champs d'affichage + rejeu).
#[derive(Debug, Clone)]
pub struct SavedSearch {
    pub id: Uuid,
    pub name: String,
    /// Payload de requête en texte JSON (à réinjecter tel quel dans `/v1/search`).
    pub query: String,
    pub notify: bool,
    /// Horodatage ISO-8601 (texte) — évite d'ajouter une feature date à sqlx.
    pub created_at: String,
}

impl Db {
    /// Enregistre une recherche et renvoie son id (tenant positionné pour la RLS).
    pub async fn create_saved_search(
        &self,
        tenant: Uuid,
        owner: Uuid,
        name: &str,
        query_json: &str,
        notify: bool,
    ) -> Result<Uuid, DbError> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("SELECT set_config('atlas.tenant', $1, true)")
            .bind(tenant.to_string())
            .execute(&mut *tx)
            .await?;
        let row = sqlx::query(
            r#"INSERT INTO saved_search (tenant_id, owner, name, query, notify)
               VALUES ($1,$2,$3,$4::jsonb,$5) RETURNING id"#,
        )
        .bind(tenant)
        .bind(owner)
        .bind(name)
        .bind(query_json)
        .bind(notify)
        .fetch_one(&mut *tx)
        .await?;
        let id = row.get::<Uuid, _>("id");
        tx.commit().await?;
        Ok(id)
    }

    /// Liste les recherches d'un propriétaire dans le tenant (récent → ancien).
    pub async fn list_saved_searches(
        &self,
        tenant: Uuid,
        owner: Uuid,
    ) -> Result<Vec<SavedSearch>, DbError> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("SELECT set_config('atlas.tenant', $1, true)")
            .bind(tenant.to_string())
            .execute(&mut *tx)
            .await?;
        let rows = sqlx::query(
            r#"SELECT id, name, query::text AS query, notify, created_at::text AS created_at
               FROM saved_search WHERE owner = $1 ORDER BY created_at DESC"#,
        )
        .bind(owner)
        .fetch_all(&mut *tx)
        .await?;
        let out = rows
            .iter()
            .map(|r| SavedSearch {
                id: r.get::<Uuid, _>("id"),
                name: r.get::<String, _>("name"),
                query: r.get::<String, _>("query"),
                notify: r.get::<bool, _>("notify"),
                created_at: r.get::<String, _>("created_at"),
            })
            .collect();
        tx.commit().await?;
        Ok(out)
    }

    /// Supprime une recherche du propriétaire ; renvoie `true` si une ligne a été supprimée.
    pub async fn delete_saved_search(
        &self,
        tenant: Uuid,
        owner: Uuid,
        id: Uuid,
    ) -> Result<bool, DbError> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("SELECT set_config('atlas.tenant', $1, true)")
            .bind(tenant.to_string())
            .execute(&mut *tx)
            .await?;
        let res = sqlx::query("DELETE FROM saved_search WHERE id = $1 AND owner = $2")
            .bind(id)
            .bind(owner)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(res.rows_affected() > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Intégration (nécessite Postgres) : prouve le cycle save → list → delete sous RLS,
    // et l'isolation inter-tenant (une recherche de t1 invisible pour t2).
    #[tokio::test]
    #[ignore = "nécessite une base de test (ATLAS_TEST_DATABASE_URL)"]
    async fn saved_search_roundtrip_and_isolation() {
        let url = std::env::var("ATLAS_TEST_DATABASE_URL").expect("ATLAS_TEST_DATABASE_URL");
        let db = Db::connect(&url).await.unwrap();
        let t1 = db.create_tenant("ss-t1").await.unwrap();
        let t2 = db.create_tenant("ss-t2").await.unwrap();
        let owner = Uuid::new_v4();

        let id = db
            .create_saved_search(
                t1,
                owner,
                "Plages",
                r#"{"query":"plage","mode":"natural"}"#,
                true,
            )
            .await
            .unwrap();

        let mine = db.list_saved_searches(t1, owner).await.unwrap();
        let found = mine
            .iter()
            .find(|s| s.id == id)
            .expect("recherche présente");
        assert_eq!(found.name, "Plages");
        assert!(found.notify);
        assert!(found.query.contains("plage"));

        // Isolation : t2 ne voit pas la recherche de t1 (RLS).
        let other = db.list_saved_searches(t2, owner).await.unwrap();
        assert!(
            !other.iter().any(|s| s.id == id),
            "fuite inter-tenant : RLS défaillante"
        );

        // Suppression idempotente.
        assert!(db.delete_saved_search(t1, owner, id).await.unwrap());
        assert!(!db.delete_saved_search(t1, owner, id).await.unwrap());
    }
}
