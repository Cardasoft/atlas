//! Atlas DAM — Core API (M1).
//! Système : /healthz, /readyz (ping DB), /version, /openapi.json (servi localement).
//! API métier sous /v1 (doc 22). Recherche : pgvector+FTS si PostgreSQL est joignable,
//! sinon index en mémoire (dégradé) pour le dev. Temps réel via WebSocket à venir (doc 40).

use axum::{extract::State, routing::get, Json, Router};
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use tower_http::services::{ServeDir, ServeFile};
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

    // Authentification de périmètre de l'API /v1 (AT-001) : clé d'API si `ATLAS_API_KEYS` est
    // fourni (identité non falsifiable), sinon mode dev/air-gap par en-têtes (mono-tenant).
    let (auth, n_keys) = atlas_search::apiauth::build_authenticator(cfg.api_keys.as_deref());
    if auth.enforces() {
        info!(
            api_keys = n_keys,
            "auth API : clés d'API requises (identité non falsifiable)"
        );
    } else {
        warn!(
            "auth API : mode DEV (en-têtes de confiance, identité FALSIFIABLE) — \
             définir ATLAS_API_KEYS pour activer l'auth par clé en production"
        );
    }

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

    let app = build_router(db, cfg.web_dir.as_deref(), auth);

    let addr: SocketAddr = cfg.bind_addr.parse()?;
    info!(%addr, "écoute");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

/// Assemble le routeur. `db = None` → recherche in-memory (dev/tests).
/// `web_dir = Some(dir)` → sert le front WASM bâti (trunk `dist/`) en statique, avec
/// repli SPA sur `index.html` ; le binaire unique sert alors l'UI **et** l'API (beta web).
/// `auth` = authentificateur de périmètre de l'API `/v1` (AT-001), appliqué à toutes les
/// routes `/v1` via une couche `Extension`.
pub fn build_router(
    db: Option<atlas_db::Db>,
    web_dir: Option<&str>,
    auth: Arc<dyn atlas_search::apiauth::ApiAuthenticator>,
) -> Router {
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
            let cache: Arc<dyn atlas_search::cache::SearchCache> = Arc::new(
                atlas_search::cache::InMemoryTtlCache::new(std::time::Duration::from_secs(60)),
            );
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
            .merge(facet_config::routes(facet_config::FacetConfigState {
                db: db.clone(),
            }))
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
                weights: Arc::new(atlas_search::StaticWeights(
                    atlas_search::rrf::Weights::default(),
                )),
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
    // Auth de périmètre (AT-001) : l'authentificateur est injecté dans chaque requête /v1 ;
    // l'extracteur `Identity` des handlers le récupère et exige une identité valide (401 sinon).
    let v1 = v1.layer(atlas_search::auth_layer(auth));

    let mut app = Router::new().merge(system).nest("/v1", v1);

    // Front WASM statique (beta web) : si `web_dir` pointe sur un `dist/` trunk existant,
    // on le sert en repli (fallback) → l'UI Leptos est servie par le même binaire que l'API.
    // Repli SPA : toute route inconnue (hors /v1, /healthz…) renvoie `index.html` (routage CSR).
    if let Some(dir) = web_dir {
        let index = Path::new(dir).join("index.html");
        if index.is_file() {
            // `.fallback` (et non `.not_found_service`) : on conserve le statut 200 du fichier
            // servi → une route SPA inconnue renvoie `index.html` en 200 (routage CSR Leptos),
            // pas un 404 (ce que ferait `not_found_service`, qui force le statut à 404).
            let serve = ServeDir::new(dir).fallback(ServeFile::new(index));
            app = app.fallback_service(serve);
            info!(web_dir = dir, "front WASM servi en statique (beta web)");
        } else {
            warn!(
                web_dir = dir,
                "ATLAS_WEB_DIR défini mais index.html introuvable — front non servi (API seule)"
            );
        }
    }

    app
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
    (
        [(axum::http::header::CONTENT_TYPE, "application/yaml")],
        SPEC,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    /// Authentificateur de dev (en-têtes) pour les tests qui n'exercent pas l'auth par clé.
    fn dev_auth() -> Arc<dyn atlas_search::apiauth::ApiAuthenticator> {
        atlas_search::apiauth::build_authenticator(None).0
    }

    #[tokio::test]
    async fn healthz_ok_without_db() {
        let app = build_router(None, None, dev_auth());
        let res = app
            .oneshot(
                Request::builder()
                    .uri("/healthz")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn search_route_mounted() {
        // Mode dev (aucune clé) : /v1/search sans auth répond 200 (mono-tenant local).
        let app = build_router(None, None, dev_auth());
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

    /// AT-001 — preuve de bout en bout : quand des clés d'API sont configurées, l'API `/v1`
    /// **exige** une clé valide. Sans `Authorization` → 401 ; mauvaise clé → 401 ; clé valide
    /// → 200. Les en-têtes de confiance ne suffisent plus (identité non falsifiable).
    #[tokio::test]
    async fn v1_requires_valid_api_key_when_configured() {
        let tenant = "66666666-6666-6666-6666-666666666666";
        let (auth, n) =
            atlas_search::apiauth::build_authenticator(Some(&format!("clef-de-test:{tenant}")));
        assert_eq!(n, 1);
        let app = build_router(None, None, auth);

        let search = |bearer: Option<&str>| {
            let mut b = Request::builder()
                .method("POST")
                .uri("/v1/search")
                .header("content-type", "application/json");
            if let Some(t) = bearer {
                b = b.header("authorization", format!("Bearer {t}"));
            }
            b.body(Body::from(r#"{"query":"mer","page_size":5}"#))
                .unwrap()
        };

        // Pas de clé → 401.
        let res = app.clone().oneshot(search(None)).await.unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

        // Mauvaise clé → 401.
        let res = app.clone().oneshot(search(Some("mauvaise"))).await.unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

        // En-tête de confiance seul (sans clé) → 401 : l'identité n'est plus falsifiable.
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/search")
                    .header("content-type", "application/json")
                    .header("x-atlas-tenant", tenant)
                    .body(Body::from(r#"{"query":"mer","page_size":5}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

        // Clé valide → 200.
        let res = app.oneshot(search(Some("clef-de-test"))).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }

    /// Beta web : avec `web_dir`, le binaire sert le front statique. On crée un `dist/`
    /// temporaire (index.html) et on vérifie qu'il est servi à la racine ET qu'une route
    /// SPA inconnue retombe sur `index.html` (routage côté client), tout en laissant l'API
    /// répondre sur ses propres routes.
    #[tokio::test]
    async fn serves_static_front_with_spa_fallback() {
        let dir = std::env::temp_dir().join(format!("atlas-web-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let marker = "<!doctype html><title>Atlas DAM test</title>";
        std::fs::write(dir.join("index.html"), marker).unwrap();

        let app = build_router(None, Some(dir.to_str().unwrap()), dev_auth());

        // Racine → index.html.
        let res = app
            .clone()
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
            .await
            .unwrap();
        assert!(String::from_utf8_lossy(&bytes).contains("Atlas DAM test"));

        // Route SPA inconnue → repli sur index.html (200, pas 404).
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/recherche/abc")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);

        // L'API garde la priorité sur le fallback statique.
        let res = app
            .oneshot(
                Request::builder()
                    .uri("/healthz")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);

        std::fs::remove_dir_all(&dir).ok();
    }
}
