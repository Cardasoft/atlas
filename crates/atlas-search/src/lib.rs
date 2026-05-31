//! atlas-search — moteur de recherche hybride (doc 04/25).
//! M1 : traits d'index (pgvector/Qdrant, FTS/OpenSearch interchangeables), fusion RRF,
//! query understanding par règles, handler `/v1/search`. Les implémentations réelles
//! (SQL) arrivent quand PostgreSQL est branché ; ici un stub en mémoire valide le flux.

pub mod rrf;
pub mod understanding;

use async_trait::async_trait;
use axum::{extract::State, routing::post, Json, Router};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use understanding::{interpret, StructuredFilter};
use uuid::Uuid;

/// Contexte d'autorisation (doc 38). M1 : porte au minimum le tenant.
#[derive(Debug, Clone)]
pub struct AuthCtx {
    pub tenant_id: Uuid,
}

/// Une voie de récupération renvoie des hits classés (doc 25 §4.3).
#[async_trait]
pub trait VectorIndex: Send + Sync {
    async fn knn(&self, query: &str, k: usize, f: &StructuredFilter, ctx: &AuthCtx) -> Vec<Uuid>;
}
#[async_trait]
pub trait LexicalIndex: Send + Sync {
    async fn search(&self, query: &str, k: usize, f: &StructuredFilter, ctx: &AuthCtx) -> Vec<Uuid>;
}

/// État injecté dans le handler (indices interchangeables).
#[derive(Clone)]
pub struct SearchState {
    pub vector: Arc<dyn VectorIndex>,
    pub lexical: Arc<dyn LexicalIndex>,
    pub weights: rrf::Weights,
}

#[derive(Debug, Deserialize)]
pub struct SearchRequest {
    pub query: String,
    #[serde(default = "default_page_size")]
    pub page_size: usize,
}
fn default_page_size() -> usize {
    50
}

#[derive(Debug, Serialize)]
pub struct SearchResultItem {
    pub asset_id: Uuid,
    pub score: f32,
}
#[derive(Debug, Serialize)]
pub struct SearchResponse {
    pub results: Vec<SearchResultItem>,
    pub interpreted_query: understanding::InterpretedQuery,
    pub degraded: bool,
}

/// Routeur du domaine recherche, monté sous `/v1` par le service core.
pub fn routes(state: SearchState) -> Router {
    Router::new()
        .route("/search", post(search_handler))
        .with_state(state)
}

/// Pipeline (doc 25 §4) : understanding → retrieval (parallèle) → fusion RRF → page.
async fn search_handler(
    State(st): State<SearchState>,
    Json(req): Json<SearchRequest>,
) -> Json<SearchResponse> {
    // M1 : tenant fixe (sera résolu depuis le jeton, doc 38).
    let ctx = AuthCtx { tenant_id: Uuid::nil() };
    let iq = interpret(&req.query);
    let k = (req.page_size * 4).max(50); // sur-récupération avant fusion

    // Voies lancées « en parallèle » (ici séquentiel sur le stub ; tokio::join! en prod).
    let v = st.vector.knn(&iq.semantic_text, k, &iq.filters, &ctx).await;
    let l = st.lexical.search(&iq.semantic_text, k, &iq.filters, &ctx).await;
    let degraded = v.is_empty(); // pas d'embedding → mode dégradé (lexical seul)

    let to_ranked = |ids: &[Uuid]| {
        ids.iter()
            .enumerate()
            .map(|(rank, &asset_id)| rrf::Ranked { asset_id, rank })
            .collect::<Vec<_>>()
    };
    let fused = rrf::fuse(&to_ranked(&v), &to_ranked(&l), &st.weights);

    let results = fused
        .into_iter()
        .take(req.page_size)
        .map(|s| SearchResultItem {
            asset_id: s.asset_id,
            score: s.score,
        })
        .collect();

    Json(SearchResponse {
        results,
        interpreted_query: iq,
        degraded,
    })
}

// --- Stub en mémoire (tests/dev ; remplacé par pgvector + FTS quand PG est branché) ---

pub struct InMemoryIndex {
    pub ids: Vec<Uuid>,
}
#[async_trait]
impl VectorIndex for InMemoryIndex {
    async fn knn(&self, _q: &str, k: usize, _f: &StructuredFilter, _c: &AuthCtx) -> Vec<Uuid> {
        self.ids.iter().take(k).copied().collect()
    }
}
#[async_trait]
impl LexicalIndex for InMemoryIndex {
    async fn search(&self, _q: &str, k: usize, _f: &StructuredFilter, _c: &AuthCtx) -> Vec<Uuid> {
        self.ids.iter().rev().take(k).copied().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn pipeline_returns_fused_results() {
        let ids: Vec<Uuid> = (1..=5).map(Uuid::from_u128).collect();
        let idx = Arc::new(InMemoryIndex { ids: ids.clone() });
        let st = SearchState {
            vector: idx.clone(),
            lexical: idx,
            weights: rrf::Weights::default(),
        };
        let resp = search_handler(
            State(st),
            Json(SearchRequest {
                query: "plage paysage sans personne".into(),
                page_size: 3,
            }),
        )
        .await;
        assert_eq!(resp.0.results.len(), 3);
        // L'understanding a bien extrait les filtres.
        assert_eq!(resp.0.interpreted_query.filters.orientation.as_deref(), Some("landscape"));
        assert!(!resp.0.degraded);
    }
}
