//! atlas-search — moteur de recherche hybride (doc 04/25).
//! M1 : traits d'index (pgvector/Qdrant, FTS/OpenSearch interchangeables), fusion RRF,
//! query understanding par règles, handler `/v1/search`. Les implémentations réelles
//! (SQL) arrivent quand PostgreSQL est branché ; ici un stub en mémoire valide le flux.

pub mod apiauth;
pub mod cache;
pub mod cursor;
pub mod eval;
pub mod rrf;
pub mod understanding;

use async_trait::async_trait;
use axum::{
    extract::{FromRequestParts, State},
    http::{request::Parts, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::Instant;
use understanding::{interpret, StructuredFilter};
use uuid::Uuid;

/// Hash déterministe d'une requête normalisée (clé d'agrégation du journal §3.2 ET liaison
/// du curseur à sa requête §4.6). `DefaultHasher` (SipHash à clés fixes) → stable entre
/// exécutions, non réversible.
fn query_hash_u64(query: &str) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    query.trim().to_lowercase().hash(&mut h);
    h.finish()
}

/// Contexte d'autorisation (doc 38). M1 : tenant + utilisateur résolus depuis la requête
/// (stand-in du jeton OIDC à venir). `Default` = tenant nil, aucun utilisateur.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AuthCtx {
    pub tenant_id: Uuid,
    /// Utilisateur résolu (propriétaire des recherches enregistrées, auteur du journal).
    /// `None` tant qu'aucune identité n'est fournie (dev/air-gap mono-utilisateur).
    pub user_id: Option<Uuid>,
}

/// Résout l'identité depuis les en-têtes (stand-in M1 du jeton, doc 38 §5). Fonction **pure**
/// testée sans HTTP : tenant absent/illisible → nil (mono-tenant dev) ; utilisateur idem → None.
/// Sera remplacée par la vérification OIDC/clé d'API en conservant cette signature.
pub fn resolve_auth(tenant_hdr: Option<&str>, user_hdr: Option<&str>) -> AuthCtx {
    AuthCtx {
        tenant_id: tenant_hdr
            .and_then(|s| Uuid::parse_str(s).ok())
            .unwrap_or_else(Uuid::nil),
        user_id: user_hdr.and_then(|s| Uuid::parse_str(s).ok()),
    }
}

/// En-têtes de **dev** portant l'identité (mode air-gap mono-tenant uniquement ; en production
/// ils sont **ignorés** au profit de la clé d'API — voir `apiauth`).
const HDR_TENANT: &str = "x-atlas-tenant";
const HDR_USER: &str = "x-atlas-user";
/// En-tête standard portant la clé d'API (`Authorization: Bearer <clé>`).
const HDR_AUTHZ: &str = "authorization";

/// Construit les [`apiauth::Credentials`] d'une requête depuis ses en-têtes (sans dépendre du
/// reste d'axum → testable). Le porteur accepte `Bearer`/`bearer`.
fn credentials_from_headers(headers: &HeaderMap) -> apiauth::Credentials {
    let get = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
    let bearer = get(HDR_AUTHZ).and_then(|v| {
        let v = v.trim();
        v.strip_prefix("Bearer ")
            .or_else(|| v.strip_prefix("bearer "))
            .map(|s| s.trim().to_string())
    });
    apiauth::Credentials {
        bearer,
        dev_tenant: get(HDR_TENANT).map(str::to_string),
        dev_user: get(HDR_USER).map(str::to_string),
    }
}

/// Rejet d'authentification → **401 Unauthorized** (corps JSON minimal + `WWW-Authenticate`).
pub struct Unauthorized;
impl IntoResponse for Unauthorized {
    fn into_response(self) -> Response {
        (
            StatusCode::UNAUTHORIZED,
            [(
                axum::http::header::WWW_AUTHENTICATE,
                "Bearer realm=\"atlas\"",
            )],
            Json(serde_json::json!({ "error": "unauthorized" })),
        )
            .into_response()
    }
}

/// Extracteur axum : résout l'identité **authentifiée** de la requête (AT-001).
///
/// L'[`apiauth::ApiAuthenticator`] est injecté par le routeur `/v1` via une couche `Extension`.
/// En mode clé (production), seule une clé d'API valide donne une identité ; sinon → 401. En
/// mode dev/air-gap, l'identité provient des en-têtes (mono-tenant). **Fail-closed** : si aucun
/// authentificateur n'est monté (ne devrait pas arriver sous `/v1`), on refuse (401) plutôt que
/// d'accorder une identité par défaut. L'autorisation fine reste portée par la RLS (défense en
/// profondeur).
#[derive(Debug, Clone)]
pub struct Identity(pub AuthCtx);

#[async_trait]
impl<S: Send + Sync> FromRequestParts<S> for Identity {
    type Rejection = Unauthorized;
    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let creds = credentials_from_headers(&parts.headers);
        match parts.extensions.get::<Arc<dyn apiauth::ApiAuthenticator>>() {
            Some(auth) => auth
                .authenticate(&creds)
                .map(Identity)
                .map_err(|_| Unauthorized),
            None => Err(Unauthorized),
        }
    }
}

/// Couche injectant l'authentificateur dans les requêtes du sous-routeur (à appliquer sur `/v1`).
/// L'extracteur [`Identity`] le récupère depuis les extensions de requête.
pub fn auth_layer(
    auth: Arc<dyn apiauth::ApiAuthenticator>,
) -> axum::Extension<Arc<dyn apiauth::ApiAuthenticator>> {
    axum::Extension(auth)
}

/// Une voie de récupération renvoie des hits classés (doc 25 §4.3).
#[async_trait]
pub trait VectorIndex: Send + Sync {
    async fn knn(&self, query: &str, k: usize, f: &StructuredFilter, ctx: &AuthCtx) -> Vec<Uuid>;
    /// kNN par l'exemple (doc 25 §4.2) : réutilise l'embedding de l'asset source.
    async fn knn_by_example(
        &self,
        example_asset_id: Uuid,
        k: usize,
        f: &StructuredFilter,
        ctx: &AuthCtx,
    ) -> Vec<Uuid>;
}
#[async_trait]
pub trait LexicalIndex: Send + Sync {
    async fn search(&self, query: &str, k: usize, f: &StructuredFilter, ctx: &AuthCtx)
        -> Vec<Uuid>;
}

/// Résumé d'asset pour l'affichage des résultats (doc 25 §5).
#[derive(Debug, Clone, Default)]
pub struct AssetSummary {
    pub title: Option<String>,
    pub rights_status: Option<String>,
}

/// Hydratation des résultats : résout les métadonnées d'affichage des assets d'une page,
/// dans le périmètre autorisé (RLS/tenant). Découplé des index pour rester interchangeable.
#[async_trait]
pub trait AssetCatalog: Send + Sync {
    async fn summaries(&self, ids: &[Uuid], ctx: &AuthCtx) -> HashMap<Uuid, AssetSummary>;
}

/// Comptage d'une valeur de facette (doc 25 §4.5).
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct FacetCount {
    pub value: String,
    pub count: u64,
}

/// Agrégations par facette : nom de facette → valeurs ordonnées (top-N borné).
/// `BTreeMap` pour une sérialisation déterministe (tests/cache stables).
pub type Facets = std::collections::BTreeMap<String, Vec<FacetCount>>;

/// Comptages de facettes sur l'**ensemble autorisé** (même clause de permission que le
/// retrieval, doc 25 §4.5). M1 : distribution du catalogue du tenant ; le filtrage par
/// facette (exclusion de sa propre dimension) viendra avec `facet_config`.
#[async_trait]
pub trait FacetProvider: Send + Sync {
    async fn facets(&self, f: &StructuredFilter, ctx: &AuthCtx) -> Facets;
}

/// Entrée du journal de recherche (doc 25 §3.2/§6) : alimente nDCG offline et popularité.
#[derive(Debug, Clone)]
pub struct SearchLogEntry {
    /// Hash déterministe de la requête normalisée (clé d'agrégation, non réversible en clair).
    pub query_hash: String,
    /// Sortie d'understanding sérialisée (jsonb côté base).
    pub interpreted_json: String,
    pub result_count: usize,
    pub latency_ms: u32,
    pub degraded: bool,
}

/// Journalise une recherche après exécution. L'écriture ne doit jamais casser la réponse :
/// les implémentations dégradent silencieusement en cas d'erreur (best-effort).
#[async_trait]
pub trait SearchLogger: Send + Sync {
    async fn log(&self, entry: SearchLogEntry, ctx: &AuthCtx);
}

/// Fournit le signal de popularité (clics agrégés) d'un lot d'assets, borné par la RLS.
/// Renvoie des **comptes bruts** par asset (assets jamais cliqués absents) ; la normalisation
/// 0..1 sur la fenêtre est faite par le pipeline avant le boost RRF (doc 25 §4.4).
#[async_trait]
pub trait PopularityProvider: Send + Sync {
    async fn popularity(&self, ids: &[Uuid], ctx: &AuthCtx) -> HashMap<Uuid, u64>;
}

/// Fournit les pondérations RRF applicables au tenant courant (doc 25 §4.4/§9). Permet de
/// configurer les poids par tenant (impl base) sans coupler le pipeline au stockage.
#[async_trait]
pub trait WeightsProvider: Send + Sync {
    async fn weights(&self, ctx: &AuthCtx) -> rrf::Weights;
}

/// Pondérations fixes (dev/air-gap sans DB, et tests) : renvoie toujours les mêmes poids.
pub struct StaticWeights(pub rrf::Weights);
#[async_trait]
impl WeightsProvider for StaticWeights {
    async fn weights(&self, _ctx: &AuthCtx) -> rrf::Weights {
        self.0.clone()
    }
}

/// État injecté dans le handler (indices + catalogue + facettes interchangeables).
#[derive(Clone)]
pub struct SearchState {
    pub vector: Arc<dyn VectorIndex>,
    pub lexical: Arc<dyn LexicalIndex>,
    pub catalog: Arc<dyn AssetCatalog>,
    pub facets: Arc<dyn FacetProvider>,
    pub logger: Arc<dyn SearchLogger>,
    pub popularity: Arc<dyn PopularityProvider>,
    pub weights: Arc<dyn WeightsProvider>,
    /// Cache de résultats cohérent avec les droits (doc 25 §6) : `NoopCache` désactive, TTL en prod.
    pub cache: Arc<dyn cache::SearchCache>,
}

/// Mode de recherche (doc 25 §4.1). `lexical` saute la voie vectorielle (filtres explicites,
/// pas d'understanding LLM). `example` (par l'exemple) réutilise l'embedding stocké de
/// l'asset source (`example_asset_id`) au lieu d'encoder un texte de requête (§4.2).
#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    #[default]
    Natural,
    Lexical,
    Example,
}
impl Mode {
    fn as_str(self) -> &'static str {
        match self {
            Mode::Natural => "natural",
            Mode::Lexical => "lexical",
            Mode::Example => "example",
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct SearchRequest {
    pub query: String,
    #[serde(default)]
    pub mode: Mode,
    /// Asset source pour `mode:"example"` (doc 25 §4.2). Ignoré dans les autres modes.
    #[serde(default)]
    pub example_asset_id: Option<Uuid>,
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
/// Fenêtre de sur-récupération avant fusion (doc 25 §4.4/§7, `k ≈ 200`). La pagination par
/// curseur se déroule **dans** cette fenêtre déterministe (re-fusion stable à requête égale,
/// §4.6) : on ne pagine pas au-delà du top-k pertinent — borne assumée d'une recherche.
const OVERFETCH_K: usize = 200;

/// Fusionne les filtres déduits (`base`) et explicites (`over`) : le client prime champ
/// par champ quand il fournit une valeur (doc 25 §4.1, filtres éditables).
fn merge_filters(base: StructuredFilter, over: StructuredFilter) -> StructuredFilter {
    StructuredFilter {
        has_people: over.has_people.or(base.has_people),
        orientation: over.orientation.or(base.orientation),
        rights_status: over.rights_status.or(base.rights_status),
        r#type: if over.r#type.is_empty() {
            base.r#type
        } else {
            over.r#type
        },
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchResultItem {
    pub asset_id: Uuid,
    pub score: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rights_status: Option<String>,
}
#[derive(Debug, Clone, Serialize)]
pub struct SearchResponse {
    pub results: Vec<SearchResultItem>,
    pub interpreted_query: understanding::InterpretedQuery,
    /// Agrégations par facette sur l'ensemble autorisé (doc 25 §4.5) ; omis si vide.
    #[serde(skip_serializing_if = "Facets::is_empty")]
    pub facets: Facets,
    /// Curseur de la page suivante (doc 25 §4.6) ; absent si plus de résultats.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    /// Hash de la requête (doc 25 §4.4/§6) : à renvoyer tel quel sur `POST /v1/search/click`
    /// pour rattacher le clic à la recherche qui l'a servi (signal de popularité).
    pub query_hash: String,
    pub degraded: bool,
}

/// Routeur du domaine recherche, monté sous `/v1` par le service core.
pub fn routes(state: SearchState) -> Router {
    Router::new()
        .route("/search", post(search_endpoint))
        .with_state(state)
}

/// Handler axum `/v1/search` : résout l'identité (en-têtes, doc 38) puis exécute le pipeline.
async fn search_endpoint(
    State(st): State<SearchState>,
    Identity(ctx): Identity,
    Json(req): Json<SearchRequest>,
) -> Json<SearchResponse> {
    Json(run_search(st, req, ctx).await)
}

/// Pipeline (doc 25 §4) : understanding → filtres éditables → retrieval (parallèle selon
/// le mode) → fusion RRF → pagination par curseur. L'identité (`ctx`) borne tout accès (RLS).
async fn run_search(st: SearchState, req: SearchRequest, ctx: AuthCtx) -> SearchResponse {
    let started = Instant::now();
    let mut iq = interpret(&req.query);
    // Filtres explicites du client → priment sur les filtres déduits (§4.1).
    if let Some(over) = req.filters.clone() {
        iq.filters = merge_filters(iq.filters.clone(), over);
    }

    let page_size = req.page_size.clamp(1, MAX_PAGE_SIZE);
    // Fenêtre fixe (§4.4) : la pagination se déroule entièrement dedans ; au moins une page.
    let k = OVERFETCH_K.max(page_size);
    let qhash = query_hash_u64(&req.query);

    // Cache cohérent avec les droits (doc 25 §6) : l'empreinte de permissions est dans la clé,
    // donc un hit ne peut jamais traverser un périmètre. Filtres explicites sérialisés tels quels.
    let filters_json = serde_json::to_string(&req.filters).unwrap_or_default();
    let ckey = cache::cache_key(
        &cache::auth_fingerprint(&ctx),
        &req.query,
        req.mode.as_str(),
        req.example_asset_id,
        &filters_json,
        page_size,
        req.cursor.as_deref(),
    );
    if let Some(hit) = st.cache.get(&ckey).await {
        return hit;
    }

    // Voies lancées « en parallèle » (ici séquentiel sur le stub ; tokio::join! en prod).
    // Lexical : saute le vectoriel (§4.1). Example : réutilise l'embedding source (§4.2).
    let v = match req.mode {
        Mode::Lexical => Vec::new(),
        Mode::Natural => st.vector.knn(&iq.semantic_text, k, &iq.filters, &ctx).await,
        Mode::Example => match req.example_asset_id {
            Some(src) => st.vector.knn_by_example(src, k, &iq.filters, &ctx).await,
            None => Vec::new(), // example sans asset source : aucune voie vectorielle possible
        },
    };
    // En mode example (par l'image), la voie lexicale n'a pas de sens : vectoriel seul.
    let l = match req.mode {
        Mode::Example => Vec::new(),
        _ => {
            st.lexical
                .search(&iq.semantic_text, k, &iq.filters, &ctx)
                .await
        }
    };
    // Facettes : comptages sur l'ensemble autorisé, même clause de permission (§4.5).
    let facets = st.facets.facets(&iq.filters, &ctx).await;
    // Dégradé = vectoriel attendu (natural/example) mais indisponible (pas le cas du lexical, §4.7).
    let degraded = matches!(req.mode, Mode::Natural | Mode::Example) && v.is_empty();

    let to_ranked = |ids: &[Uuid]| {
        ids.iter()
            .enumerate()
            .map(|(rank, &asset_id)| rrf::Ranked { asset_id, rank })
            .collect::<Vec<_>>()
    };
    // Pondérations du tenant (doc 25 §9) ; défauts neutres si non configurées.
    let weights = st.weights.weights(&ctx).await;
    let mut fused = rrf::fuse(&to_ranked(&v), &to_ranked(&l), &weights);

    // Boost de popularité (§4.4) : appliqué sur la fenêtre fusionnée, avant pagination, donc
    // déterministe à requête égale → curseur stable. Interrogé seulement si le poids est actif
    // (évite un aller-retour base inutile). Normalisé 0..1 par le max de la fenêtre.
    if weights.popularity != 0.0 {
        let ids: Vec<Uuid> = fused.iter().map(|s| s.asset_id).collect();
        let counts = st.popularity.popularity(&ids, &ctx).await;
        let normalized = normalize_popularity(counts);
        rrf::apply_popularity(&mut fused, &normalized, weights.popularity);
    }

    // Pagination stable par curseur (§4.6) : pas d'OFFSET, ordre fusionné déterministe.
    // Un curseur d'une AUTRE requête (query_hash différent) est ignoré → page 1.
    let cursor = req
        .cursor
        .as_deref()
        .and_then(cursor::Cursor::decode)
        .filter(|c| c.query_hash == qhash);
    let (page, next) = cursor::paginate(&fused, cursor, page_size, qhash);

    // Hydratation : métadonnées d'affichage de la seule page (§5), dans le périmètre autorisé.
    let page_ids: Vec<Uuid> = page.iter().map(|s| s.asset_id).collect();
    let mut summaries = st.catalog.summaries(&page_ids, &ctx).await;
    let results = page
        .into_iter()
        .map(|s| {
            let sum = summaries.remove(&s.asset_id).unwrap_or_default();
            SearchResultItem {
                asset_id: s.asset_id,
                score: s.score,
                title: sum.title,
                rights_status: sum.rights_status,
            }
        })
        .collect();

    // Journalisation best-effort (doc 25 §3.2/§6) : ne casse jamais la réponse.
    st.logger
        .log(
            SearchLogEntry {
                query_hash: format!("{qhash:016x}"),
                interpreted_json: serde_json::to_string(&iq).unwrap_or_else(|_| "{}".into()),
                result_count: fused.len(),
                latency_ms: started.elapsed().as_millis() as u32,
                degraded,
            },
            &ctx,
        )
        .await;

    let response = SearchResponse {
        results,
        interpreted_query: iq,
        facets,
        next_cursor: next.map(|c| c.encode()),
        query_hash: format!("{qhash:016x}"),
        degraded,
    };

    // Mise en cache (doc 25 §6) : on ne mémorise pas une réponse dégradée (vectoriel indisponible),
    // qui ne reflète pas le résultat nominal et serait servie à tort jusqu'à expiration du TTL.
    if !degraded {
        st.cache.put(ckey, ctx.tenant_id, response.clone()).await;
    }
    response
}

/// Normalise des comptes de clics bruts en scores 0..1 (fraction du max de la fenêtre).
/// Fenêtre vide ou max nul → table vide (boost neutre).
fn normalize_popularity(counts: HashMap<Uuid, u64>) -> HashMap<Uuid, f32> {
    let max = counts.values().copied().max().unwrap_or(0);
    if max == 0 {
        return HashMap::new();
    }
    counts
        .into_iter()
        .map(|(id, c)| (id, c as f32 / max as f32))
        .collect()
}

/// Shim de test : exécute le pipeline avec une identité par défaut (tenant nil, mono-utilisateur),
/// en conservant la forme `(State, Json) -> Json` historique des tests du handler.
#[cfg(test)]
async fn search_handler(
    State(st): State<SearchState>,
    Json(req): Json<SearchRequest>,
) -> Json<SearchResponse> {
    Json(run_search(st, req, AuthCtx::default()).await)
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
    async fn knn_by_example(
        &self,
        example_asset_id: Uuid,
        k: usize,
        _f: &StructuredFilter,
        _c: &AuthCtx,
    ) -> Vec<Uuid> {
        // Stub : « voisins » = tous les ids sauf la source (l'ordre n'a pas de sens ici).
        self.ids
            .iter()
            .filter(|&&id| id != example_asset_id)
            .take(k)
            .copied()
            .collect()
    }
}
#[async_trait]
impl LexicalIndex for InMemoryIndex {
    async fn search(&self, _q: &str, k: usize, _f: &StructuredFilter, _c: &AuthCtx) -> Vec<Uuid> {
        self.ids.iter().rev().take(k).copied().collect()
    }
}

/// Catalogue sans métadonnées (dev/air-gap sans DB) : les résultats portent uniquement
/// `asset_id` + `score`. Remplacé par un catalogue adossé à la base quand PostgreSQL est branché.
pub struct NoopCatalog;
#[async_trait]
impl AssetCatalog for NoopCatalog {
    async fn summaries(&self, _ids: &[Uuid], _ctx: &AuthCtx) -> HashMap<Uuid, AssetSummary> {
        HashMap::new()
    }
}

/// Fournisseur de facettes vide (dev/air-gap sans DB) : aucune agrégation.
pub struct NoopFacets;
#[async_trait]
impl FacetProvider for NoopFacets {
    async fn facets(&self, _f: &StructuredFilter, _ctx: &AuthCtx) -> Facets {
        Facets::new()
    }
}

/// Journal de recherche inerte (dev/air-gap sans DB) : ne persiste rien.
pub struct NoopSearchLog;
#[async_trait]
impl SearchLogger for NoopSearchLog {
    async fn log(&self, _entry: SearchLogEntry, _ctx: &AuthCtx) {}
}

/// Fournisseur de popularité inerte (dev/air-gap sans DB) : aucun signal de clic.
pub struct NoopPopularity;
#[async_trait]
impl PopularityProvider for NoopPopularity {
    async fn popularity(&self, _ids: &[Uuid], _ctx: &AuthCtx) -> HashMap<Uuid, u64> {
        HashMap::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Catalogue de test : renvoie un titre dérivé de l'id et un statut de droits fixe.
    struct FakeCatalog;
    #[async_trait]
    impl AssetCatalog for FakeCatalog {
        async fn summaries(&self, ids: &[Uuid], _ctx: &AuthCtx) -> HashMap<Uuid, AssetSummary> {
            ids.iter()
                .map(|&id| {
                    (
                        id,
                        AssetSummary {
                            title: Some(format!("asset-{}", id.as_u128())),
                            rights_status: Some("valid".into()),
                        },
                    )
                })
                .collect()
        }
    }

    /// Fournisseur de facettes de test : une distribution fixe d'orientation.
    struct FakeFacets;
    #[async_trait]
    impl FacetProvider for FakeFacets {
        async fn facets(&self, _f: &StructuredFilter, _ctx: &AuthCtx) -> Facets {
            let mut m = Facets::new();
            m.insert(
                "orientation".into(),
                vec![
                    FacetCount {
                        value: "landscape".into(),
                        count: 3,
                    },
                    FacetCount {
                        value: "portrait".into(),
                        count: 1,
                    },
                ],
            );
            m
        }
    }

    /// Journal de test : capture les entrées dans un buffer partagé.
    #[derive(Clone, Default)]
    struct FakeLogger {
        entries: Arc<std::sync::Mutex<Vec<SearchLogEntry>>>,
    }
    #[async_trait]
    impl SearchLogger for FakeLogger {
        async fn log(&self, entry: SearchLogEntry, _ctx: &AuthCtx) {
            self.entries.lock().unwrap().push(entry);
        }
    }

    fn state_with(ids: Vec<Uuid>) -> SearchState {
        state_full(
            ids,
            Arc::new(NoopCatalog),
            Arc::new(NoopFacets),
            Arc::new(NoopSearchLog),
        )
    }

    fn state_with_catalog(ids: Vec<Uuid>, catalog: Arc<dyn AssetCatalog>) -> SearchState {
        state_full(ids, catalog, Arc::new(NoopFacets), Arc::new(NoopSearchLog))
    }

    fn state_full(
        ids: Vec<Uuid>,
        catalog: Arc<dyn AssetCatalog>,
        facets: Arc<dyn FacetProvider>,
        logger: Arc<dyn SearchLogger>,
    ) -> SearchState {
        let idx = Arc::new(InMemoryIndex { ids });
        SearchState {
            vector: idx.clone(),
            lexical: idx,
            catalog,
            facets,
            logger,
            popularity: Arc::new(NoopPopularity),
            weights: Arc::new(StaticWeights(rrf::Weights::default())),
            cache: Arc::new(cache::NoopCache),
        }
    }

    fn req(query: &str) -> SearchRequest {
        SearchRequest {
            query: query.into(),
            mode: Mode::Natural,
            example_asset_id: None,
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
            Json(SearchRequest {
                page_size: 3,
                ..req("plage paysage sans personne")
            }),
        )
        .await;
        assert_eq!(resp.0.results.len(), 3);
        // L'understanding a bien extrait les filtres.
        assert_eq!(
            resp.0.interpreted_query.filters.orientation.as_deref(),
            Some("landscape")
        );
        assert!(!resp.0.degraded);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn lexical_mode_skips_vector_and_is_not_degraded() {
        let ids: Vec<Uuid> = (1..=4).map(Uuid::from_u128).collect();
        let resp = search_handler(
            State(state_with(ids)),
            Json(SearchRequest {
                mode: Mode::Lexical,
                ..req("mer")
            }),
        )
        .await;
        // Lexical seul → résultats présents, et PAS marqué dégradé (omission volontaire, §4.7).
        assert!(!resp.0.results.is_empty());
        assert!(!resp.0.degraded);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn explicit_filters_override_deduced_ones() {
        // "mer" ne déduit aucune orientation ; le client en impose une → elle doit primer.
        let over = StructuredFilter {
            orientation: Some("portrait".into()),
            ..Default::default()
        };
        let resp = search_handler(
            State(state_with(vec![Uuid::from_u128(1)])),
            Json(SearchRequest {
                filters: Some(over),
                ..req("mer")
            }),
        )
        .await;
        assert_eq!(
            resp.0.interpreted_query.filters.orientation.as_deref(),
            Some("portrait")
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cursor_paginates_without_overlap() {
        let ids: Vec<Uuid> = (1..=5).map(Uuid::from_u128).collect();
        let mut seen = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let resp = search_handler(
                State(state_with(ids.clone())),
                Json(SearchRequest {
                    page_size: 2,
                    cursor: cursor.clone(),
                    ..req("mer")
                }),
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
        assert_eq!(
            uniq.len(),
            ids.len(),
            "aucun doublon ni saut sur la pagination"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn results_are_hydrated_with_metadata() {
        let id = Uuid::from_u128(7);
        let resp = search_handler(
            State(state_with_catalog(vec![id], Arc::new(FakeCatalog))),
            Json(req("mer")),
        )
        .await;
        let item = resp
            .0
            .results
            .iter()
            .find(|r| r.asset_id == id)
            .expect("résultat présent");
        assert_eq!(item.title.as_deref(), Some("asset-7"));
        assert_eq!(item.rights_status.as_deref(), Some("valid"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn noop_catalog_leaves_metadata_empty() {
        let resp = search_handler(
            State(state_with(vec![Uuid::from_u128(1)])),
            Json(req("mer")),
        )
        .await;
        assert!(resp.0.results[0].title.is_none());
        assert!(resp.0.results[0].rights_status.is_none());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn facets_are_returned_in_response() {
        let resp = search_handler(
            State(state_full(
                vec![Uuid::from_u128(1)],
                Arc::new(NoopCatalog),
                Arc::new(FakeFacets),
                Arc::new(NoopSearchLog),
            )),
            Json(req("mer")),
        )
        .await;
        let orient = resp
            .0
            .facets
            .get("orientation")
            .expect("facette orientation présente");
        assert_eq!(
            orient[0],
            FacetCount {
                value: "landscape".into(),
                count: 3
            }
        );
        assert_eq!(orient.len(), 2);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn noop_facets_leave_facets_empty() {
        let resp = search_handler(
            State(state_with(vec![Uuid::from_u128(1)])),
            Json(req("mer")),
        )
        .await;
        assert!(resp.0.facets.is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn search_is_logged_with_metrics() {
        let logger = Arc::new(FakeLogger::default());
        let st = state_full(
            (1..=3).map(Uuid::from_u128).collect(),
            Arc::new(NoopCatalog),
            Arc::new(NoopFacets),
            logger.clone(),
        );
        let _ = search_handler(State(st), Json(req("plage paysage"))).await;
        let entries = logger.entries.lock().unwrap();
        assert_eq!(entries.len(), 1, "une entrée de journal par recherche");
        let e = &entries[0];
        assert_eq!(e.result_count, 3);
        assert!(!e.degraded);
        assert!(!e.query_hash.is_empty());
        // L'interprétation sérialisée contient le filtre déduit (orientation paysage).
        assert!(e.interpreted_json.contains("landscape"));
    }

    /// Fournisseur de popularité de test : comptes de clics fixes par asset.
    struct FakePopularity(HashMap<Uuid, u64>);
    #[async_trait]
    impl PopularityProvider for FakePopularity {
        async fn popularity(&self, ids: &[Uuid], _ctx: &AuthCtx) -> HashMap<Uuid, u64> {
            ids.iter()
                .filter_map(|id| self.0.get(id).map(|&c| (*id, c)))
                .collect()
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn popularity_signal_reorders_results() {
        let ids: Vec<Uuid> = (1..=3).map(Uuid::from_u128).collect();
        let last = ids[2];
        // Le dernier asset est massivement cliqué → doit remonter en tête une fois le boost actif.
        let idx = Arc::new(InMemoryIndex { ids: ids.clone() });
        let st = SearchState {
            vector: idx.clone(),
            lexical: idx,
            catalog: Arc::new(NoopCatalog),
            facets: Arc::new(NoopFacets),
            logger: Arc::new(NoopSearchLog),
            popularity: Arc::new(FakePopularity(HashMap::from([(last, 100)]))),
            weights: Arc::new(StaticWeights(rrf::Weights {
                semantic: 1.0,
                lexical: 1.0,
                popularity: 5.0,
            })),
            cache: Arc::new(cache::NoopCache),
        };
        let resp = search_handler(State(st), Json(req("mer"))).await;
        assert_eq!(
            resp.0.results[0].asset_id, last,
            "le plus cliqué remonte en tête"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn popularity_not_queried_when_weight_zero() {
        // Poids nul → aucun appel au fournisseur ; un fournisseur qui panique resterait inerte.
        struct PanicPopularity;
        #[async_trait]
        impl PopularityProvider for PanicPopularity {
            async fn popularity(&self, _ids: &[Uuid], _ctx: &AuthCtx) -> HashMap<Uuid, u64> {
                panic!("ne doit pas être interrogé quand le poids est nul");
            }
        }
        let idx = Arc::new(InMemoryIndex {
            ids: (1..=3).map(Uuid::from_u128).collect(),
        });
        let st = SearchState {
            vector: idx.clone(),
            lexical: idx,
            catalog: Arc::new(NoopCatalog),
            facets: Arc::new(NoopFacets),
            logger: Arc::new(NoopSearchLog),
            popularity: Arc::new(PanicPopularity),
            weights: Arc::new(StaticWeights(rrf::Weights::default())), // popularity = 0
            cache: Arc::new(cache::NoopCache),
        };
        let resp = search_handler(State(st), Json(req("mer"))).await;
        assert!(!resp.0.results.is_empty());
    }

    #[test]
    fn query_hash_is_stable_and_normalized() {
        assert_eq!(query_hash_u64("Plage "), query_hash_u64("plage"));
        assert_ne!(query_hash_u64("plage"), query_hash_u64("montagne"));
    }

    #[test]
    fn resolve_auth_parses_valid_headers() {
        let t = Uuid::from_u128(42);
        let u = Uuid::from_u128(7);
        let ctx = resolve_auth(Some(&t.to_string()), Some(&u.to_string()));
        assert_eq!(ctx.tenant_id, t);
        assert_eq!(ctx.user_id, Some(u));
    }

    #[test]
    fn resolve_auth_defaults_when_absent_or_invalid() {
        // Absents → tenant nil, utilisateur None.
        let ctx = resolve_auth(None, None);
        assert_eq!(ctx.tenant_id, Uuid::nil());
        assert_eq!(ctx.user_id, None);
        // Illisibles → mêmes défauts (jamais d'échec, RLS reste la garde).
        let ctx = resolve_auth(Some("pas-un-uuid"), Some("non-plus"));
        assert_eq!(ctx.tenant_id, Uuid::nil());
        assert_eq!(ctx.user_id, None);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn foreign_cursor_is_ignored_returns_first_page() {
        let ids: Vec<Uuid> = (1..=5).map(Uuid::from_u128).collect();
        // Page 1 de référence (sans curseur).
        let p1 = search_handler(
            State(state_with(ids.clone())),
            Json(SearchRequest {
                page_size: 2,
                ..req("mer")
            }),
        )
        .await;
        let want: Vec<Uuid> = p1.0.results.iter().map(|r| r.asset_id).collect();
        // Curseur forgé pour une AUTRE requête (query_hash différent) → doit être ignoré.
        let foreign = cursor::Cursor {
            score: 0.0,
            asset_id: Uuid::nil(),
            query_hash: query_hash_u64("mer").wrapping_add(1),
        };
        let resp = search_handler(
            State(state_with(ids)),
            Json(SearchRequest {
                page_size: 2,
                cursor: Some(foreign.encode()),
                ..req("mer")
            }),
        )
        .await;
        let got: Vec<Uuid> = resp.0.results.iter().map(|r| r.asset_id).collect();
        assert_eq!(got, want, "curseur étranger ignoré → page 1 servie");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn example_mode_uses_source_embedding_and_excludes_source() {
        let ids: Vec<Uuid> = (1..=4).map(Uuid::from_u128).collect();
        let src = ids[0];
        let resp = search_handler(
            State(state_with(ids.clone())),
            Json(SearchRequest {
                mode: Mode::Example,
                example_asset_id: Some(src),
                ..req("")
            }),
        )
        .await;
        // Par l'exemple : des voisins reviennent, et la source est exclue des résultats.
        assert!(!resp.0.results.is_empty());
        assert!(
            resp.0.results.iter().all(|r| r.asset_id != src),
            "la source ne doit pas figurer"
        );
        assert!(!resp.0.degraded);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn example_mode_without_source_is_degraded_and_empty() {
        let resp = search_handler(
            State(state_with((1..=3).map(Uuid::from_u128).collect())),
            Json(SearchRequest {
                mode: Mode::Example,
                example_asset_id: None,
                ..req("")
            }),
        )
        .await;
        assert!(resp.0.results.is_empty());
        assert!(resp.0.degraded, "example sans asset source → dégradé");
    }

    // État avec cache TTL réel + journal capturant : le nombre d'entrées journalisées compte les
    // calculs effectifs (un hit retourne avant la journalisation), ce qui révèle hit vs miss.
    fn state_with_cache(
        ids: Vec<Uuid>,
        logger: FakeLogger,
        cache: Arc<dyn cache::SearchCache>,
    ) -> SearchState {
        let idx = Arc::new(InMemoryIndex { ids });
        SearchState {
            vector: idx.clone(),
            lexical: idx,
            catalog: Arc::new(NoopCatalog),
            facets: Arc::new(NoopFacets),
            logger: Arc::new(logger),
            popularity: Arc::new(NoopPopularity),
            weights: Arc::new(StaticWeights(rrf::Weights::default())),
            cache,
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cache_hit_skips_recomputation() {
        let logger = FakeLogger::default();
        let cache = Arc::new(cache::InMemoryTtlCache::new(
            std::time::Duration::from_secs(60),
        ));
        let st = state_with_cache(
            (1..=3).map(Uuid::from_u128).collect(),
            logger.clone(),
            cache,
        );
        // 1er appel : miss → calcul + journalisation.
        let _ = run_search(st.clone(), req("mer"), AuthCtx::default()).await;
        // 2e appel identique : hit → retour anticipé, aucune nouvelle journalisation.
        let _ = run_search(st, req("mer"), AuthCtx::default()).await;
        assert_eq!(
            logger.entries.lock().unwrap().len(),
            1,
            "le 2e appel est servi par le cache"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cache_isolates_by_permission_fingerprint() {
        let logger = FakeLogger::default();
        let cache = Arc::new(cache::InMemoryTtlCache::new(
            std::time::Duration::from_secs(60),
        ));
        let st = state_with_cache(
            (1..=3).map(Uuid::from_u128).collect(),
            logger.clone(),
            cache,
        );
        let a = AuthCtx {
            tenant_id: Uuid::nil(),
            user_id: Some(Uuid::from_u128(10)),
        };
        let b = AuthCtx {
            tenant_id: Uuid::nil(),
            user_id: Some(Uuid::from_u128(20)),
        };
        let _ = run_search(st.clone(), req("mer"), a.clone()).await; // miss (A)
        let _ = run_search(st.clone(), req("mer"), b).await; // périmètre B distinct → miss
        let _ = run_search(st, req("mer"), a).await; // même périmètre que A → hit
        assert_eq!(
            logger.entries.lock().unwrap().len(),
            2,
            "deux périmètres ⇒ deux calculs (pas de fuite)"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn degraded_response_is_not_cached() {
        let logger = FakeLogger::default();
        let cache = Arc::new(cache::InMemoryTtlCache::new(
            std::time::Duration::from_secs(60),
        ));
        // Index vide → mode natural dégradé (vectoriel attendu mais vide).
        let st = state_with_cache(vec![], logger.clone(), cache);
        let r1 = run_search(st.clone(), req("mer"), AuthCtx::default()).await;
        assert!(r1.degraded);
        let _ = run_search(st, req("mer"), AuthCtx::default()).await;
        assert_eq!(
            logger.entries.lock().unwrap().len(),
            2,
            "réponse dégradée non mémorisée → recalcul"
        );
    }
}
