//! Endpoints de configuration des pondérations RRF (doc 25 §4.4/§9).
//! `GET /v1/search/weights` et `PUT /v1/search/weights`. Disponibles uniquement si PostgreSQL
//! est branché. Pilotent l'équilibre sémantique/lexical/popularité de la fusion hybride pour
//! le tenant. Tenant résolu depuis l'identité (doc 38) ; défauts neutres si non configuré.

use atlas_search::Identity;
use axum::{extract::State, http::StatusCode, routing::get, Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Clone)]
pub struct WeightsState {
    pub db: atlas_db::Db,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SearchWeights {
    #[serde(default = "default_semantic")]
    pub semantic: f32,
    #[serde(default = "default_lexical")]
    pub lexical: f32,
    #[serde(default)]
    pub popularity: f32,
}
fn default_semantic() -> f32 {
    1.0
}
fn default_lexical() -> f32 {
    1.0
}

pub fn routes(state: WeightsState) -> Router {
    Router::new()
        .route("/search/weights", get(get_weights).put(put_weights))
        .with_state(state)
}

async fn get_weights(
    State(st): State<WeightsState>,
    Identity(ctx): Identity,
) -> Result<Json<SearchWeights>, (StatusCode, Json<Value>)> {
    let w = st
        .db
        .get_search_weights(ctx.tenant_id)
        .await
        .map_err(internal)?;
    // Non configuré → défauts neutres (cohérent avec le pipeline).
    let (semantic, lexical, popularity) = w.unwrap_or((1.0, 1.0, 0.0));
    Ok(Json(SearchWeights {
        semantic,
        lexical,
        popularity,
    }))
}

async fn put_weights(
    State(st): State<WeightsState>,
    Identity(ctx): Identity,
    Json(w): Json<SearchWeights>,
) -> Result<StatusCode, (StatusCode, Json<Value>)> {
    st.db
        .put_search_weights(ctx.tenant_id, w.semantic, w.lexical, w.popularity)
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
