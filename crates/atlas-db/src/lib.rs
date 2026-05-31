//! atlas-db — accès PostgreSQL souverain (doc 02/38).
//! Pool sqlx + isolation multi-tenant par RLS (`set_config('atlas.tenant', …, true)`,
//! équivalent à `SET LOCAL` : ne vaut que pour la transaction → aucune fuite inter-tenant).
//! M1 : recherche lexicale FTS opérationnelle ; le vectoriel (pgvector) suit.

use sqlx::postgres::{PgPool, PgPoolOptions};
use sqlx::Row;
use std::time::Duration;
use uuid::Uuid;

pub mod repo;
pub mod saved;
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

    /// Métadonnées d'affichage d'un lot d'assets (doc 25 §5), bornées par la RLS du tenant.
    /// Renvoie `(id, title, rights_status)` pour les ids visibles dans le périmètre.
    pub async fn asset_summaries(
        &self,
        tenant: Uuid,
        ids: &[Uuid],
    ) -> Result<Vec<(Uuid, Option<String>, String)>, DbError> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut tx = self.pool.begin().await?;
        sqlx::query("SELECT set_config('atlas.tenant', $1, true)")
            .bind(tenant.to_string())
            .execute(&mut *tx)
            .await?;
        let rows = sqlx::query(
            "SELECT id, title, rights_status FROM asset WHERE id = ANY($1)",
        )
        .bind(ids)
        .fetch_all(&mut *tx)
        .await?;
        let out = rows
            .iter()
            .map(|r| {
                (
                    r.get::<Uuid, _>("id"),
                    r.get::<Option<String>, _>("title"),
                    r.get::<String, _>("rights_status"),
                )
            })
            .collect();
        tx.commit().await?;
        Ok(out)
    }

    /// Comptages de facettes sur l'ensemble autorisé du tenant (doc 25 §4.5).
    /// `GROUP BY` borné à `top_n` valeurs par facette ; renvoie `(facette, [(valeur, count)])`.
    /// Les colonnes sont des littéraux figés (aucune interpolation d'entrée → pas d'injection).
    pub async fn facet_counts(
        &self,
        tenant: Uuid,
        top_n: i64,
    ) -> Result<Vec<(String, Vec<(String, i64)>)>, DbError> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("SELECT set_config('atlas.tenant', $1, true)")
            .bind(tenant.to_string())
            .execute(&mut *tx)
            .await?;

        let mut out = Vec::new();
        // Facettes textuelles : orientation, rights_status, mime. Colonnes figées.
        for (facet, col) in [
            ("orientation", "orientation"),
            ("rights_status", "rights_status"),
            ("mime", "mime"),
        ] {
            let sql = format!(
                "SELECT {col}::text AS v, count(*) AS c FROM asset \
                 WHERE {col} IS NOT NULL GROUP BY {col} ORDER BY c DESC, v ASC LIMIT $1"
            );
            let rows = sqlx::query(&sql).bind(top_n).fetch_all(&mut *tx).await?;
            let vals = rows
                .iter()
                .map(|r| (r.get::<String, _>("v"), r.get::<i64, _>("c")))
                .collect::<Vec<_>>();
            if !vals.is_empty() {
                out.push((facet.to_string(), vals));
            }
        }

        // Facette booléenne has_people : valeurs « true »/« false ».
        let rows = sqlx::query(
            "SELECT has_people::text AS v, count(*) AS c FROM asset \
             WHERE has_people IS NOT NULL GROUP BY has_people ORDER BY c DESC, v ASC LIMIT $1",
        )
        .bind(top_n)
        .fetch_all(&mut *tx)
        .await?;
        let vals = rows
            .iter()
            .map(|r| (r.get::<String, _>("v"), r.get::<i64, _>("c")))
            .collect::<Vec<_>>();
        if !vals.is_empty() {
            out.push(("has_people".to_string(), vals));
        }

        tx.commit().await?;
        Ok(out)
    }

    /// Journalise une recherche (doc 25 §3.2/§6). `interpreted` est inséré en jsonb via cast
    /// `$::jsonb` (pas de feature sqlx json). N'échoue jamais silencieusement côté appelant :
    /// l'erreur remonte mais l'index la dégrade (le log ne doit pas casser la recherche).
    #[allow(clippy::too_many_arguments)]
    pub async fn insert_search_log(
        &self,
        tenant: Uuid,
        user_id: Option<Uuid>,
        query_hash: &str,
        interpreted_json: &str,
        result_count: i32,
        latency_ms: Option<i32>,
        degraded: bool,
    ) -> Result<(), DbError> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("SELECT set_config('atlas.tenant', $1, true)")
            .bind(tenant.to_string())
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            r#"INSERT INTO search_log
                 (tenant_id, user_id, query_hash, interpreted, result_count, latency_ms, degraded)
               VALUES ($1,$2,$3,$4::jsonb,$5,$6,$7)"#,
        )
        .bind(tenant)
        .bind(user_id)
        .bind(query_hash)
        .bind(interpreted_json)
        .bind(result_count)
        .bind(latency_ms)
        .bind(degraded)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }
}
