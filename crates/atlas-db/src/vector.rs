//! Recherche vectorielle pgvector (doc 25 §4.3).
//!
//! TDD : `pgvector_literal` est une fonction **pure** testée sans base (les tests
//! décrivent le format attendu du littéral `vector`). `Db::vector_search` exécute le
//! kNN avec RLS + filtres ; son test est une intégration `#[ignore]` (nécessite Postgres).

use crate::{Db, DbError};
use atlas_search::understanding::StructuredFilter;
use sqlx::Row;
use uuid::Uuid;

/// Sérialise un vecteur f32 en littéral pgvector : `[v1,v2,...]`.
/// (pgvector accepte le cast `'[...]'::vector`.)
pub fn pgvector_literal(v: &[f32]) -> String {
    let mut s = String::with_capacity(v.len() * 8 + 2);
    s.push('[');
    for (i, x) in v.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push_str(&x.to_string());
    }
    s.push(']');
    s
}

impl Db {
    /// kNN approximatif (HNSW, distance cosinus) borné par la RLS du tenant + filtres.
    pub async fn vector_search(
        &self,
        tenant: Uuid,
        qvec: &[f32],
        filter: &StructuredFilter,
        k: i64,
    ) -> Result<Vec<Uuid>, DbError> {
        let lit = pgvector_literal(qvec);
        let orientation = filter.orientation.clone();
        let rights = filter.rights_status.clone();
        let has_people = filter.has_people;

        let mut tx = self.pool.begin().await?;
        sqlx::query("SELECT set_config('atlas.tenant', $1, true)")
            .bind(tenant.to_string())
            .execute(&mut *tx)
            .await?;

        // Filtres poussés dans la requête (pré-filtrage) ; RLS gère le tenant.
        let rows = sqlx::query(
            r#"
            SELECT e.asset_id
            FROM embedding e
            JOIN asset a ON a.id = e.asset_id
            WHERE e.kind = 'multimodal'
              AND ($2::bool IS NULL OR a.has_people = $2)
              AND ($3::text IS NULL OR a.orientation = $3)
              AND ($4::text IS NULL OR a.rights_status = $4)
            ORDER BY e.vec <=> $1::vector
            LIMIT $5
            "#,
        )
        .bind(lit)
        .bind(has_people)
        .bind(orientation)
        .bind(rights)
        .bind(k)
        .fetch_all(&mut *tx)
        .await?;

        let ids = rows.iter().map(|r| r.get::<Uuid, _>("asset_id")).collect();
        tx.commit().await?;
        Ok(ids)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_formats_vector() {
        assert_eq!(pgvector_literal(&[1.0, -0.5, 0.0]), "[1,-0.5,0]");
    }

    #[test]
    fn literal_handles_empty() {
        assert_eq!(pgvector_literal(&[]), "[]");
    }

    #[test]
    fn literal_single_element() {
        assert_eq!(pgvector_literal(&[0.25]), "[0.25]");
    }
}
