//! Implémentations PostgreSQL des index de recherche (doc 25).
//! - PgLexicalIndex : FTS opérationnel.
//! - PgVectorIndex : squelette ; renverra des résultats kNN une fois l'embedding
//!   de requête calculé (mode dégradé = vide en attendant → lexical seul).

use async_trait::async_trait;
use atlas_embed::Embedder;
use atlas_search::{
    rrf::Weights, understanding::StructuredFilter, AssetCatalog, AssetSummary, AuthCtx, FacetCount,
    FacetProvider, Facets, LexicalIndex, PopularityProvider, SearchLogEntry, SearchLogger,
    VectorIndex, WeightsProvider,
};
use std::collections::HashMap;
use std::sync::Arc;
use uuid::Uuid;

use crate::Db;

const DEFAULT_LANG: &str = "simple";
/// Top-N borné par facette (doc 25 §4.5, `GROUP BY` borné).
const FACET_TOP_N: i64 = 20;
/// Périmètre de facettes par défaut en M1 (rôle/espace/portail à venir, doc 38).
const DEFAULT_FACET_SCOPE: &str = "tenant";

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

    async fn knn_by_example(
        &self,
        example_asset_id: Uuid,
        k: usize,
        f: &StructuredFilter,
        ctx: &AuthCtx,
    ) -> Vec<Uuid> {
        // Réutilise le vecteur stocké de l'asset source (aucun encodage), doc 25 §4.2.
        match self
            .db
            .vector_search_by_example(ctx.tenant_id, example_asset_id, f, k as i64)
            .await
        {
            Ok(ids) => ids,
            Err(e) => {
                tracing::warn!(error = %e, "vector_search_by_example a échoué");
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
        let rows = match self.db.facet_counts(ctx.tenant_id, FACET_TOP_N).await {
            Ok(rows) => rows,
            Err(e) => {
                // Dégradation gracieuse : pas de facettes plutôt qu'échec de la recherche.
                tracing::warn!(error = %e, "facet_counts a échoué");
                return Facets::new();
            }
        };

        let to_counts = |vals: Vec<(String, i64)>| -> Vec<FacetCount> {
            vals.into_iter()
                .map(|(value, count)| FacetCount { value, count: count.max(0) as u64 })
                .collect()
        };

        // Config de facettes du périmètre : restreint les facettes restituées à la liste
        // configurée (l'ordre d'un objet JSON n'est pas significatif → seul le filtrage l'est).
        // Absente (ou en erreur) → on expose toutes les facettes calculées (défaut, §4.5).
        let fields = self
            .db
            .facet_config_fields(ctx.tenant_id, DEFAULT_FACET_SCOPE)
            .await
            .unwrap_or_default();

        if fields.is_empty() {
            return rows.into_iter().map(|(f, v)| (f, to_counts(v))).collect();
        }

        let mut computed: HashMap<String, Vec<(String, i64)>> = rows.into_iter().collect();
        let mut out = Facets::new();
        for field in fields {
            if let Some(vals) = computed.remove(&field) {
                out.insert(field, to_counts(vals));
            }
        }
        out
    }
}

/// Fournisseur de popularité adossé à PostgreSQL : clics agrégés depuis `search_log` sous RLS.
pub struct PgPopularity {
    pub db: Db,
}

#[async_trait]
impl PopularityProvider for PgPopularity {
    async fn popularity(&self, ids: &[Uuid], ctx: &AuthCtx) -> HashMap<Uuid, u64> {
        match self.db.asset_popularity(ctx.tenant_id, ids).await {
            Ok(rows) => rows.into_iter().map(|(id, c)| (id, c.max(0) as u64)).collect(),
            Err(e) => {
                // Dégradation gracieuse : pas de boost plutôt qu'échec de la recherche.
                tracing::warn!(error = %e, "asset_popularity a échoué");
                HashMap::new()
            }
        }
    }
}

/// Fournisseur de pondérations RRF adossé à PostgreSQL : poids configurés par tenant (RLS).
/// Tenant non configuré ou erreur → défauts neutres (la recherche reste fonctionnelle).
pub struct PgWeights {
    pub db: Db,
}

#[async_trait]
impl WeightsProvider for PgWeights {
    async fn weights(&self, ctx: &AuthCtx) -> Weights {
        match self.db.get_search_weights(ctx.tenant_id).await {
            Ok(Some((semantic, lexical, popularity))) => Weights { semantic, lexical, popularity },
            Ok(None) => Weights::default(),
            Err(e) => {
                tracing::warn!(error = %e, "get_search_weights a échoué — défauts appliqués");
                Weights::default()
            }
        }
    }
}

/// Journal de recherche adossé à PostgreSQL (doc 25 §3.2/§6). Best-effort : une erreur
/// d'écriture est tracée mais n'affecte pas la réponse de recherche.
pub struct PgSearchLog {
    pub db: Db,
}

#[async_trait]
impl SearchLogger for PgSearchLog {
    async fn log(&self, entry: SearchLogEntry, ctx: &AuthCtx) {
        if let Err(e) = self
            .db
            .insert_search_log(
                ctx.tenant_id,
                ctx.user_id, // résolu depuis l'identité de la requête (doc 38)
                &entry.query_hash,
                &entry.interpreted_json,
                entry.result_count as i32,
                Some(entry.latency_ms as i32),
                entry.degraded,
            )
            .await
        {
            tracing::warn!(error = %e, "insert_search_log a échoué");
        }
    }
}
