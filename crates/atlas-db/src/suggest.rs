//! Autocomplétion de recherche (doc 25 §5).
//!
//! M1 : suggestions issues des **titres d'assets** par préfixe (ILIKE), bornées par la RLS.
//! Le `search_log` ne stocke qu'un `query_hash` (irréversible) → pas de suggestions par
//! popularité des requêtes passées en M1.
//!
//! TDD : `like_prefix_pattern` est une fonction **pure** (échappement LIKE) testée sans base ;
//! `Db::suggest_titles` est validée par un test d'intégration `#[ignore]`.

use crate::{Db, DbError};
use sqlx::Row;
use uuid::Uuid;

/// Construit un motif LIKE de **préfixe** sûr : échappe les métacaractères `\`, `%`, `_`
/// (avec `ESCAPE '\'`) puis ajoute `%`. Ainsi l'entrée utilisateur est traitée littéralement,
/// seul le `%` final reste un joker. Exemple : `a%b_c` → `a\%b\_c%`.
pub fn like_prefix_pattern(prefix: &str) -> String {
    let mut out = String::with_capacity(prefix.len() + 1);
    for ch in prefix.chars() {
        match ch {
            '\\' | '%' | '_' => {
                out.push('\\');
                out.push(ch);
            }
            _ => out.push(ch),
        }
    }
    out.push('%');
    out
}

impl Db {
    /// Suggère des titres d'assets commençant par `prefix` (ILIKE, insensible à la casse),
    /// bornés par la RLS du tenant. Tri par **popularité** (clics agrégés par titre, doc 25
    /// §5) décroissante puis ordre alphabétique pour départager. Limité à `limit`.
    pub async fn suggest_titles(
        &self,
        tenant: Uuid,
        prefix: &str,
        limit: i64,
    ) -> Result<Vec<String>, DbError> {
        let pattern = like_prefix_pattern(prefix);
        let mut tx = self.pool.begin().await?;
        sqlx::query("SELECT set_config('atlas.tenant', $1, true)")
            .bind(tenant.to_string())
            .execute(&mut *tx)
            .await?;
        // Popularité agrégée par titre : on additionne les clics de tous les assets partageant
        // le titre. `GROUP BY title` dédoublonne (équivaut au DISTINCT précédent).
        let rows = sqlx::query(
            r#"SELECT a.title,
                      COALESCE(SUM(pop.cnt), 0) AS popularity
               FROM asset a
               LEFT JOIN (
                   SELECT c.aid AS asset_id, count(*) AS cnt
                   FROM search_log sl, unnest(sl.clicked) AS c(aid)
                   GROUP BY c.aid
               ) pop ON pop.asset_id = a.id
               WHERE a.title IS NOT NULL AND a.title ILIKE $1 ESCAPE '\'
               GROUP BY a.title
               ORDER BY popularity DESC, a.title ASC
               LIMIT $2"#,
        )
        .bind(pattern)
        .bind(limit)
        .fetch_all(&mut *tx)
        .await?;
        let out = rows.iter().map(|r| r.get::<String, _>("title")).collect();
        tx.commit().await?;
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_appends_wildcard() {
        assert_eq!(like_prefix_pattern("plage"), "plage%");
    }

    #[test]
    fn prefix_escapes_metacharacters() {
        assert_eq!(like_prefix_pattern("a%b_c"), "a\\%b\\_c%");
    }

    #[test]
    fn prefix_escapes_backslash() {
        assert_eq!(like_prefix_pattern("a\\b"), "a\\\\b%");
    }

    #[test]
    fn prefix_empty_matches_all() {
        assert_eq!(like_prefix_pattern(""), "%");
    }

    // --- Intégration (nécessite Postgres) : `ATLAS_TEST_DATABASE_URL=... cargo test -- --ignored`
    #[tokio::test]
    #[ignore = "nécessite une base de test (ATLAS_TEST_DATABASE_URL)"]
    async fn suggest_titles_by_prefix_under_rls() {
        let url = std::env::var("ATLAS_TEST_DATABASE_URL").expect("ATLAS_TEST_DATABASE_URL");
        let db = Db::connect(&url).await.unwrap();
        let t1 = db.create_tenant("sg1").await.unwrap();
        let t2 = db.create_tenant("sg2").await.unwrap();

        db.insert_asset(
            t1,
            "Plage au coucher de soleil",
            "image/jpeg",
            "READY",
            "valid",
            None,
            None,
            &atlas_types::Provenance::default(),
        )
        .await
        .unwrap();
        db.insert_asset(
            t1,
            "Plage de galets",
            "image/jpeg",
            "READY",
            "valid",
            None,
            None,
            &atlas_types::Provenance::default(),
        )
        .await
        .unwrap();
        db.insert_asset(
            t1,
            "Montagne enneigée",
            "image/jpeg",
            "READY",
            "valid",
            None,
            None,
            &atlas_types::Provenance::default(),
        )
        .await
        .unwrap();
        // Asset d'un autre tenant : ne doit JAMAIS remonter (RLS).
        db.insert_asset(
            t2,
            "Plage secrète t2",
            "image/jpeg",
            "READY",
            "valid",
            None,
            None,
            &atlas_types::Provenance::default(),
        )
        .await
        .unwrap();

        let sug = db.suggest_titles(t1, "plage", 8).await.unwrap();
        assert_eq!(sug, vec!["Plage au coucher de soleil", "Plage de galets"]);
        assert!(
            !sug.iter().any(|s| s.contains("t2")),
            "fuite inter-tenant (RLS)"
        );
    }
}
