//! Atlas DAM — Core API (M1).
//! Système : /healthz, /readyz (ping DB), /version, /openapi.json (servi localement).
//! API métier sous /v1 (doc 22). Recherche : pgvector+FTS si PostgreSQL est joignable,
//! sinon index en mémoire (dégradé) pour le dev. Temps réel via WebSocket à venir (doc 40).

use axum::{extract::State, routing::get, Json, Router};
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::sync::Arc;
use tracing::{info, warn};

mod assets;
mod click;
mod facet_config;
mod searches;
mod suggest;
mod weights;

/// État des routes système (readiness dépend de la DB si présente).
#[derive(Clone)]
struct AppState {
    db: Option<atlas_db::Db>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let cfg = atlas_config::Config::from_env()?;
    info!(edition = ?cfg.edition, external_llm = cfg.allow_external_llm, "démarrage atlas-core");

    // Connexion DB optionnelle : le service démarre même sans PG (dev/air-gap test).
    let db = match atlas_db::Db::connect(&cfg.database_url).await {
        Ok(db) => {
            info!("PostgreSQL connecté");
            Some(db)
        }
        Err(e) => {
            warn!(error = %e, "PostgreSQL indisponible — recherche en mode dégradé (in-memory)");
            None
        }
    };

    let app = build_router(db);

    let addr: SocketAddr = cfg.bind_addr.parse()?;
    info!(%addr, "écoute");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

/// Assemble le routeur. `db = None` → recherche in-memory (dev/tests).
pub fn build_router(db: Option<atlas_db::Db>) -> Router {
    // M1 : encodeur factice déterministe, partagé recherche + ingestion.
    // Remplacé par SigLIP (ort/Candle) en conservant le trait Embedder → aucun changement aval.
    let embedder: Arc<dyn atlas_embed::Embedder> = Arc::new(atlas_embed::FakeEmbedder);

    // Hub temps réel (WebSocket) : toujours présent ; publie les événements d'ingestion.
    let hub = atlas_realtime::Hub::new();

    // Sélection des index + routes d'ingestion selon la disponibilité de PostgreSQL.
    let (search_state, ingest_routes): (atlas_search::SearchState, Option<Router>) = match &db {
        Some(db) => {
            // Cache de résultats cohérent avec les droits (doc 25 §6), TTL court en mémoire.
            // Partagé entre recherche (lecture) et ingestion (purge du tenant) → même instance.
            let cache: Arc<dyn atlas_search::cache::SearchCache> =
                Arc::new(atlas_search::cache::InMemoryTtlCache::new(std::time::Duration::from_secs(60)));
            let search_state = atlas_search::SearchState {
                vector: Arc::new(atlas_db::search_pg::PgVectorIndex {
                    db: db.clone(),
                    embedder: embedder.clone(),
                }),
                lexical: Arc::new(atlas_db::search_pg::PgLexicalIndex { db: db.clone() }),
                catalog: Arc::new(atlas_db::search_pg::PgAssetCatalog { db: db.clone() }),
                facets: Arc::new(atlas_db::search_pg::PgFacets { db: db.clone() }),
                logger: Arc::new(atlas_db::search_pg::PgSearchLog { db: db.clone() }),
                popularity: Arc::new(atlas_db::search_pg::PgPopularity { db: db.clone() }),
                weights: Arc::new(atlas_db::search_pg::PgWeights { db: db.clone() }),
                cache: cache.clone(),
            };
            let ingest = assets::routes(assets::AssetsState {
                db: db.clone(),
                embedder: embedder.clone(),
                hub: hub.clone(),
                cache,
            })
            // Recherches enregistrées : disponibles avec la DB (doc 25 §3.2).
            .merge(searches::routes(searches::SearchesState { db: db.clone() }))
            // Configuration des facettes : pilote les facettes calculées (doc 25 §4.5).
            .merge(facet_config::routes(facet_config::FacetConfigState { db: db.clone() }))
            // Autocomplétion : suggestions de titres par préfixe (doc 25 §5).
            .merge(suggest::routes(suggest::SuggestState { db: db.clone() }))
            // Capture de clic : alimente le signal de popularité (doc 25 §4.4/§6).
            .merge(click::routes(click::ClickState { db: db.clone() }))
            // Pondérations RRF : équilibre sémantique/lexical/popularité par tenant (§4.4/§9).
            .merge(weights::routes(weights::WeightsState { db: db.clone() }));
            (search_state, Some(ingest))
        }
        None => {
            let idx = Arc::new(atlas_search::InMemoryIndex { ids: vec![] });
            let search_state = atlas_search::SearchState {
                vector: idx.clone(),
                lexical: idx,
                catalog: Arc::new(atlas_search::NoopCatalog),
                facets: Arc::new(atlas_search::NoopFacets),
                logger: Arc::new(atlas_search::NoopSearchLog),
                popularity: Arc::new(atlas_search::NoopPopularity),
                weights: Arc::new(atlas_search::StaticWeights(atlas_search::rrf::Weights::default())),
                cache: Arc::new(atlas_search::cache::NoopCache), // dev/air-gap : pas de cache
            };
            (search_state, None) // ingestion indisponible sans DB
        }
    };

    let system = Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .route("/version", get(version))
        .route("/openapi.json", get(openapi))
        .with_state(AppState { db });

    // /v1 = recherche (toujours) + WebSocket temps réel (toujours) + ingestion (si DB).
    let mut v1 = atlas_search::routes(search_state).merge(atlas_realtime::routes(hub));
    if let Some(ingest) = ingest_routes {
        v1 = v1.merge(ingest);
    }

    Router::new().merge(system).nest("/v1", v1)
}

async fn healthz() -> Json<Value> {
    Json(json!({ "status": "ok" }))
}

/// Readiness : vérifie la DB si configurée.
async fn readyz(State(app): State<AppState>) -> Json<Value> {
    let db_ok = match &app.db {
        Some(db) => db.ping().await.is_ok(),
        None => false,
    };
    Json(json!({
        "status": if db_ok || app.db.is_none() { "ready" } else { "degraded" },
        "checks": { "self": "ok", "database": db_ok }
    }))
}

async fn version() -> Json<Value> {
    Json(json!({ "product": "Atlas DAM", "version": env!("CARGO_PKG_VERSION"), "api": "v1" }))
}

async fn openapi() -> ([(axum::http::HeaderName, &'static str); 1], &'static str) {
    const SPEC: &str = include_str!("../../../openapi/atlas.v1.yaml");
    ([(axum::http::header::CONTENT_TYPE, "application/yaml")], SPEC)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    #[tokio::test]
    async fn healthz_ok_without_db() {
        let app = build_router(None);
        let res = app
            .oneshot(Request::builder().uri("/healthz").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn search_route_mounted() {
        let app = build_router(None);
        let res = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/search")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"query":"mer paysage","page_size":5}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }
}
