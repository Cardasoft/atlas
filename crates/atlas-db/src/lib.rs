//! atlas-db — accès PostgreSQL souverain (doc 02/38).
//! Pool sqlx + isolation multi-tenant par RLS (`set_config('atlas.tenant', …, true)`,
//! équivalent à `SET LOCAL` : ne vaut que pour la transaction → aucune fuite inter-tenant).
//! M1 : recherche lexicale FTS opérationnelle ; le vectoriel (pgvector) suit.

use sqlx::postgres::{PgPool, PgPoolOptions};
use sqlx::Row;
use std::time::Duration;
use uuid::Uuid;

pub mod repo;
pub mod search_pg;
pub mod vector;

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),
}

/// Façade base de données (clonable : le pool est partagé).
#[derive(Clone)]
pub struct Db {
    pub pool: PgPool,
}

impl Db {
    /// Connecte un pool frugal (profil Solo par défaut).
    pub async fn connect(url: &str) -> Result<Self, DbError> {
        let pool = PgPoolOptions::new()
            .max_connections(10)
            .acquire_timeout(Duration::from_secs(5))
            .connect(url)
            .await?;
        Ok(Self { pool })
    }

    /// Readiness : ping simple (`/readyz`, doc 21).
    pub async fn ping(&self) -> Result<(), DbError> {
        sqlx::query("SELECT 1").execute(&self.pool).await?;
        Ok(())
    }

    /// Recherche lexicale FTS, bornée par la RLS du tenant (doc 25 §4.4).
    /// La transaction positionne `atlas.tenant` avant la requête → isolation garantie.
    pub async fn lexical_search(
        &self,
        tenant: Uuid,
        terms: &str,
        lang: &str,
        k: i64,
    ) -> Result<Vec<Uuid>, DbError> {
        let mut tx = self.pool.begin().await?;

        sqlx::query("SELECT set_config('atlas.tenant', $1, true)")
            .bind(tenant.to_string())
            .execute(&mut *tx)
            .await?;

        let rows = sqlx::query(
            r#"
            SELECT st.asset_id
            FROM search_text st
            WHERE st.tsv @@ websearch_to_tsquery($1, $2)
            ORDER BY ts_rank(st.tsv, websearch_to_tsquery($1, $2)) DESC
            LIMIT $3
            "#,
        )
        .bind(lang)
        .bind(terms)
        .bind(k)
        .fetch_all(&mut *tx)
        .await?;

        let ids = rows.iter().map(|r| r.get::<Uuid, _>("asset_id")).collect();
        tx.commit().await?;
        Ok(ids)
    }
}
