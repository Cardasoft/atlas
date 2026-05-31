//! atlas-search — moteur de recherche hybride (doc 04/25).
//! M1 : traits d'index (pgvector/Qdrant, FTS/OpenSearch interchangeables), fusion RRF,
//! query understanding par règles, handler `/v1/search`. Les implémentations réelles
//! (SQL) arrivent quand PostgreSQL est branché ; ici un stub en mémoire valide le flux.

pub mod cursor;
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

/// Mode de recherche (doc 25 §4.1). `lexical` saute la voie vectorielle (filtres explicites,
/// pas d'understanding LLM). `example` (par l'exemple) viendra avec la réutilisation
/// d'embedding de l'asset source (dépend de la DB).
#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    #[default]
    Natural,
    Lexical,
}

#[derive(Debug, Deserialize)]
pub struct SearchRequest {
    pub query: String,
    #[serde(default)]
    pub mode: Mode,
    /// Filtres explicites : priment sur les filtres déduits (interpreted_query éditable, §4.1).
    #[serde(default)]
    pub filters: Option<StructuredFilter>,
    #[serde(default = "default_page_size")]
    pub page_size: usize,
    /// Curseur opaque de pagination (doc 25 §4.6). Absent → première page.
    #[serde(default)]
    pub cursor: Option<String>,
}
fn default_page_size() -> usize {
    50
}
const MAX_PAGE_SIZE: usize = 200;

/// Fusionne les filtres déduits (`base`) et explicites (`over`) : le client prime champ
/// par champ quand il fournit une valeur (doc 25 §4.1, filtres éditables).
fn merge_filters(base: StructuredFilter, over: StructuredFilter) -> StructuredFilter {
    StructuredFilter {
        has_people: over.has_people.or(base.has_people),
        orientation: over.orientation.or(base.orientation),
        rights_status: over.rights_status.or(base.rights_status),
        r#type: if over.r#type.is_empty() { base.r#type } else { over.r#type },
    }
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
    /// Curseur de la page suivante (doc 25 §4.6) ; absent si plus de résultats.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    pub degraded: bool,
}

/// Routeur du domaine recherche, monté sous `/v1` par le service core.
pub fn routes(state: SearchState) -> Router {
    Router::new()
        .route("/search", post(search_handler))
        .with_state(state)
}

/// Pipeline (doc 25 §4) : understanding → filtres éditables → retrieval (parallèle selon
/// le mode) → fusion RRF → pagination par curseur.
async fn search_handler(
    State(st): State<SearchState>,
    Json(req): Json<SearchRequest>,
) -> Json<SearchResponse> {
    // M1 : tenant fixe (sera résolu depuis le jeton, doc 38).
    let ctx = AuthCtx { tenant_id: Uuid::nil() };
    let mut iq = interpret(&req.query);
    // Filtres explicites du client → priment sur les filtres déduits (§4.1).
    if let Some(over) = req.filters.clone() {
        iq.filters = merge_filters(iq.filters.clone(), over);
    }

    let page_size = req.page_size.clamp(1, MAX_PAGE_SIZE);
    let k = (page_size * 4).max(50); // sur-récupération avant fusion

    // Voies lancées « en parallèle » (ici séquentiel sur le stub ; tokio::join! en prod).
    // En mode lexical, on saute volontairement la voie vectorielle (§4.1).
    let v = match req.mode {
        Mode::Lexical => Vec::new(),
        Mode::Natural => st.vector.knn(&iq.semantic_text, k, &iq.filters, &ctx).await,
    };
    let l = st.lexical.search(&iq.semantic_text, k, &iq.filters, &ctx).await;
    // Dégradé = vectoriel attendu mais indisponible (pas le cas du lexical explicite, §4.7).
    let degraded = matches!(req.mode, Mode::Natural) && v.is_empty();

    let to_ranked = |ids: &[Uuid]| {
        ids.iter()
            .enumerate()
            .map(|(rank, &asset_id)| rrf::Ranked { asset_id, rank })
            .collect::<Vec<_>>()
    };
    let fused = rrf::fuse(&to_ranked(&v), &to_ranked(&l), &st.weights);

    // Pagination stable par curseur (§4.6) : pas d'OFFSET, ordre fusionné déterministe.
    let cursor = req.cursor.as_deref().and_then(cursor::Cursor::decode);
    let (page, next) = cursor::paginate(&fused, cursor, page_size);

    let results = page
        .into_iter()
        .map(|s| SearchResultItem {
            asset_id: s.asset_id,
            score: s.score,
        })
        .collect();

    Json(SearchResponse {
        results,
        interpreted_query: iq,
        next_cursor: next.map(|c| c.encode()),
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

    fn state_with(ids: Vec<Uuid>) -> SearchState {
        let idx = Arc::new(InMemoryIndex { ids });
        SearchState {
            vector: idx.clone(),
            lexical: idx,
            weights: rrf::Weights::default(),
        }
    }

    fn req(query: &str) -> SearchRequest {
        SearchRequest {
            query: query.into(),
            mode: Mode::Natural,
            filters: None,
            page_size: 50,
            cursor: None,
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn pipeline_returns_fused_results() {
        let ids: Vec<Uuid> = (1..=5).map(Uuid::from_u128).collect();
        let resp = search_handler(
            State(state_with(ids)),
            Json(SearchRequest { page_size: 3, ..req("plage paysage sans personne") }),
        )
        .await;
        assert_eq!(resp.0.results.len(), 3);
        // L'understanding a bien extrait les filtres.
        assert_eq!(resp.0.interpreted_query.filters.orientation.as_deref(), Some("landscape"));
        assert!(!resp.0.degraded);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn lexical_mode_skips_vector_and_is_not_degraded() {
        let ids: Vec<Uuid> = (1..=4).map(Uuid::from_u128).collect();
        let resp = search_handler(
            State(state_with(ids)),
            Json(SearchRequest { mode: Mode::Lexical, ..req("mer") }),
        )
        .await;
        // Lexical seul → résultats présents, et PAS marqué dégradé (omission volontaire, §4.7).
        assert!(!resp.0.results.is_empty());
        assert!(!resp.0.degraded);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn explicit_filters_override_deduced_ones() {
        // "mer" ne déduit aucune orientation ; le client en impose une → elle doit primer.
        let over = StructuredFilter { orientation: Some("portrait".into()), ..Default::default() };
        let resp = search_handler(
            State(state_with(vec![Uuid::from_u128(1)])),
            Json(SearchRequest { filters: Some(over), ..req("mer") }),
        )
        .await;
        assert_eq!(resp.0.interpreted_query.filters.orientation.as_deref(), Some("portrait"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cursor_paginates_without_overlap() {
        let ids: Vec<Uuid> = (1..=5).map(Uuid::from_u128).collect();
        let mut seen = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let resp = search_handler(
                State(state_with(ids.clone())),
                Json(SearchRequest { page_size: 2, cursor: cursor.clone(), ..req("mer") }),
            )
            .await;
            seen.extend(resp.0.results.iter().map(|r| r.asset_id));
            match resp.0.next_cursor {
                Some(c) => cursor = Some(c),
                None => break,
            }
        }
        // Tous les assets vus une seule fois (les voies vec+lex portent le même set d'ids).
        let mut uniq = seen.clone();
        uniq.sort();
        uniq.dedup();
        assert_eq!(uniq.len(), ids.len(), "aucun doublon ni saut sur la pagination");
    }
}
