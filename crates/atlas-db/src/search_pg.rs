//! Implémentations PostgreSQL des index de recherche (doc 25).
//! - PgLexicalIndex : FTS opérationnel.
//! - PgVectorIndex : squelette ; renverra des résultats kNN une fois l'embedding
//!   de requête calculé (mode dégradé = vide en attendant → lexical seul).

use async_trait::async_trait;
use atlas_embed::Embedder;
use atlas_search::{
    understanding::StructuredFilter, AssetCatalog, AssetSummary, AuthCtx, FacetCount, FacetProvider,
    Facets, LexicalIndex, VectorIndex,
};
use std::collections::HashMap;
use std::sync::Arc;
use uuid::Uuid;

use crate::Db;

const DEFAULT_LANG: &str = "simple";
/// Top-N borné par facette (doc 25 §4.5, `GROUP BY` borné).
const FACET_TOP_N: i64 = 20;

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

/// Catalogue d'assets adossé à PostgreSQL : hydrate les résultats (titre, droits) sous RLS.
pub struct PgAssetCatalog {
    pub db: Db,
}

#[async_trait]
impl AssetCatalog for PgAssetCatalog {
    async fn summaries(&self, ids: &[Uuid], ctx: &AuthCtx) -> HashMap<Uuid, AssetSummary> {
        match self.db.asset_summaries(ctx.tenant_id, ids).await {
            Ok(rows) => rows
                .into_iter()
                .map(|(id, title, rights_status)| {
                    (id, AssetSummary { title, rights_status: Some(rights_status) })
                })
                .collect(),
            Err(e) => {
                // Dégradation gracieuse : résultats sans métadonnées plutôt qu'échec total.
                tracing::warn!(error = %e, "asset_summaries a échoué");
                HashMap::new()
            }
        }
    }
}

/// Fournisseur de facettes adossé à PostgreSQL : agrège sur l'ensemble autorisé sous RLS.
pub struct PgFacets {
    pub db: Db,
}

#[async_trait]
impl FacetProvider for PgFacets {
    async fn facets(&self, _f: &StructuredFilter, ctx: &AuthCtx) -> Facets {
        match self.db.facet_counts(ctx.tenant_id, FACET_TOP_N).await {
            Ok(rows) => rows
                .into_iter()
                .map(|(facet, vals)| {
                    let counts = vals
                        .into_iter()
                        .map(|(value, count)| FacetCount { value, count: count.max(0) as u64 })
                        .collect();
                    (facet, counts)
                })
                .collect(),
            Err(e) => {
                // Dégradation gracieuse : pas de facettes plutôt qu'échec de la recherche.
                tracing::warn!(error = %e, "facet_counts a échoué");
                Facets::new()
            }
        }
    }
}
