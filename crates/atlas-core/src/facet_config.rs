//! Endpoints de configuration des facettes (doc 25 §4.5 / §5).
//! `GET /v1/search/facet-config?scope=…` et `PUT /v1/search/facet-config`.
//! Disponibles uniquement si PostgreSQL est branché. Pilote quelles facettes la recherche
//! calcule pour le périmètre. M1 : tenant fixe (résolu depuis le jeton à terme, doc 38).

use atlas_search::Identity;
use axum::{
    extract::{Query, State},
    http::StatusCode,
    routing::get,
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Clone)]
pub struct FacetConfigState {
    pub db: atlas_db::Db,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct FacetConfig {
    #[serde(default = "default_scope")]
    pub scope: String,
    /// Liste ordonnée de champs de facette (p.ex. ["mime","orientation","rights_status"]).
    pub facets: Vec<String>,
}
fn default_scope() -> String {
    "tenant".into()
}

#[derive(Debug, Deserialize)]
pub struct ScopeQuery {
    #[serde(default = "default_scope")]
    scope: String,
}

pub fn routes(state: FacetConfigState) -> Router {
    Router::new()
        .route("/search/facet-config", get(get_config).put(put_config))
        .with_state(state)
}

async fn get_config(
    State(st): State<FacetConfigState>,
    Identity(ctx): Identity,
    Query(q): Query<ScopeQuery>,
) -> Result<Json<FacetConfig>, (StatusCode, Json<Value>)> {
    let raw = st
        .db
        .get_facet_config(ctx.tenant_id, &q.scope)
        .await
        .map_err(internal)?;
    // Texte JSON issu de jsonb → tableau valide ; défaut [] si non configuré.
    let facets = raw
        .and_then(|s| serde_json::from_str::<Vec<String>>(&s).ok())
        .unwrap_or_default();
    Ok(Json(FacetConfig {
        scope: q.scope,
        facets,
    }))
}

async fn put_config(
    State(st): State<FacetConfigState>,
    Identity(ctx): Identity,
    Json(cfg): Json<FacetConfig>,
) -> Result<StatusCode, (StatusCode, Json<Value>)> {
    let facets_json = serde_json::to_string(&cfg.facets).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "type": "https://atlas.local/errors/bad-request",
                "title": "Facettes invalides",
                "detail": e.to_string()
            })),
        )
    })?;
    st.db
        .put_facet_config(ctx.tenant_id, &cfg.scope, &facets_json)
        .await
        .map_err(internal)?;
    Ok(StatusCode::NO_CONTENT)
}

fn internal(e: atlas_db::DbError) -> (StatusCode, Json<Value>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({
            "type": "https://atlas.local/errors/internal",
            "title": "Erreur interne",
            "detail": e.to_string()
        })),
    )
}
