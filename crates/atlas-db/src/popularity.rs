//! Signal de popularité issu des clics (doc 25 §4.4 / §6).
//!
//! Quand un utilisateur ouvre un résultat, l'asset est ajouté au tableau `clicked` de la
//! **dernière** ligne `search_log` portant le même `query_hash` (la recherche qui l'a servi).
//! Ce signal alimente deux usages, tous deux bornés par la RLS du tenant :
//!   1. le **tri des suggestions** d'autocomplétion (titres les plus cliqués d'abord) ;
//!   2. le **boost de popularité** de la fusion RRF (`asset_popularity`).
//!
//! Le `query_hash` est irréversible : on ne stocke jamais le texte de la requête.

use crate::{Db, DbError};
use sqlx::Row;
use uuid::Uuid;

impl Db {
    /// Enregistre un clic : ajoute `asset_id` au `clicked` de la dernière recherche du tenant
    /// portant `query_hash` (RLS). Renvoie `true` si une ligne a été mise à jour (`false` si
    /// aucune recherche correspondante — clic orphelin, ignoré sans erreur).
    pub async fn record_click(
        &self,
        tenant: Uuid,
        query_hash: &str,
        asset_id: Uuid,
    ) -> Result<bool, DbError> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("SELECT set_config('atlas.tenant', $1, true)")
            .bind(tenant.to_string())
            .execute(&mut *tx)
            .await?;
        let res = sqlx::query(
            r#"UPDATE search_log SET clicked = array_append(clicked, $1)
               WHERE id = (
                   SELECT id FROM search_log
                   WHERE query_hash = $2
                   ORDER BY created_at DESC
                   LIMIT 1
               )"#,
        )
        .bind(asset_id)
        .bind(query_hash)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(res.rows_affected() > 0)
    }

    /// Compte les clics par asset sur le périmètre du tenant (RLS), restreint à `ids`.
    /// Renvoie `(asset_id, count)` ; les assets jamais cliqués sont simplement absents.
    pub async fn asset_popularity(
        &self,
        tenant: Uuid,
        ids: &[Uuid],
    ) -> Result<Vec<(Uuid, i64)>, DbError> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut tx = self.pool.begin().await?;
        sqlx::query("SELECT set_config('atlas.tenant', $1, true)")
            .bind(tenant.to_string())
            .execute(&mut *tx)
            .await?;
        let rows = sqlx::query(
            r#"SELECT c.aid AS asset_id, count(*) AS cnt
               FROM search_log sl, unnest(sl.clicked) AS c(aid)
               WHERE c.aid = ANY($1)
               GROUP BY c.aid"#,
        )
        .bind(ids)
        .fetch_all(&mut *tx)
        .await?;
        let out = rows
            .iter()
            .map(|r| (r.get::<Uuid, _>("asset_id"), r.get::<i64, _>("cnt")))
            .collect();
        tx.commit().await?;
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Intégration (nécessite Postgres) : `ATLAS_TEST_DATABASE_URL=... cargo test -- --ignored`
    #[tokio::test]
    #[ignore = "nécessite une base de test (ATLAS_TEST_DATABASE_URL)"]
    async fn record_click_and_popularity_under_rls() {
        let url = std::env::var("ATLAS_TEST_DATABASE_URL").expect("ATLAS_TEST_DATABASE_URL");
        let db = Db::connect(&url).await.unwrap();
        let t1 = db.create_tenant("pop1").await.unwrap();

        let a1 = db
            .insert_asset(t1, "Plage", "image/jpeg", "READY", "valid", None, None)
            .await
            .unwrap();
        let a2 = db
            .insert_asset(t1, "Montagne", "image/jpeg", "READY", "valid", None, None)
            .await
            .unwrap();

        // Deux recherches portant le même hash : le clic vise la plus récente.
        let qh = "deadbeefcafe0001";
        db.insert_search_log(t1, None, qh, "{}", 2, Some(5), false).await.unwrap();
        db.insert_search_log(t1, None, qh, "{}", 2, Some(5), false).await.unwrap();

        assert!(db.record_click(t1, qh, a1).await.unwrap(), "clic enregistré");
        assert!(db.record_click(t1, qh, a1).await.unwrap());
        assert!(db.record_click(t1, qh, a2).await.unwrap());
        // Hash inconnu → aucun enregistrement.
        assert!(!db.record_click(t1, "0000000000000000", a1).await.unwrap());

        let pop = db.asset_popularity(t1, &[a1, a2]).await.unwrap();
        let cnt = |id: Uuid| pop.iter().find(|(i, _)| *i == id).map(|(_, c)| *c).unwrap_or(0);
        assert_eq!(cnt(a1), 2);
        assert_eq!(cnt(a2), 1);
    }
}
