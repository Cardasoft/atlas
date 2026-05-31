//! Implémentations PostgreSQL des index de recherche (doc 25).
//! - PgLexicalIndex : FTS opérationnel.
//! - PgVectorIndex : squelette ; renverra des résultats kNN une fois l'embedding
//!   de requête calculé (mode dégradé = vide en attendant → lexical seul).

use async_trait::async_trait;
use atlas_embed::Embedder;
use atlas_search::{understanding::StructuredFilter, AuthCtx, LexicalIndex, VectorIndex};
use std::sync::Arc;
use uuid::Uuid;

use crate::Db;

const DEFAULT_LANG: &str = "simple";

/// Index lexical adossé à PostgreSQL FTS.
pub struct PgLexicalIndex {
    pub db: Db,
}

#[async_trait]
impl LexicalIndex for PgLexicalIndex {
    async fn search(&self, query: &str, k: usize, _f: &StructuredFilter, ctx: &AuthCtx) -> Vec<Uuid> {
        match self
            .db
            .lexical_search(ctx.tenant_id, query, DEFAULT_LANG, k as i64)
            .await
        {
            Ok(ids) => ids,
            Err(e) => {
                // Dégradation gracieuse : une erreur lexicale ne casse pas la recherche.
                tracing::warn!(error = %e, "lexical_search a échoué");
                Vec::new()
            }
        }
    }
}

/// Index vectoriel adossé à pgvector, avec encodeur de requête in-process (doc 25 §4.2-4.3).
pub struct PgVectorIndex {
    pub db: Db,
    pub embedder: Arc<dyn Embedder>,
}

#[async_trait]
impl VectorIndex for PgVectorIndex {
    async fn knn(&self, query: &str, k: usize, f: &StructuredFilter, ctx: &AuthCtx) -> Vec<Uuid> {
        // 1) embedding de la requête (texte → espace multimodal), in-process.
        let qvec = self.embedder.encode(query);
        // 2) kNN HNSW + filtres + RLS.
        match self.db.vector_search(ctx.tenant_id, &qvec, f, k as i64).await {
            Ok(ids) => ids,
            Err(e) => {
                // Dégradation gracieuse : on retombe sur le lexical (doc 04 §3.3).
                tracing::warn!(error = %e, "vector_search a échoué");
                Vec::new()
            }
        }
    }
}
