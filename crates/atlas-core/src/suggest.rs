//! Endpoint d'autocomplétion (doc 25 §5) : `GET /v1/search/suggest?q=&limit=`.
//! Disponible uniquement si PostgreSQL est branché. M1 : suggestions issues des titres
//! d'assets par préfixe, bornées par la RLS. Tenant fixe (résolu depuis le jeton, doc 38).

use axum::{
    extract::{Query, State},
    http::StatusCode,
    routing::get,
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

#[derive(Clone)]
pub struct SuggestState {
    pub db: atlas_db::Db,
}

const DEFAULT_LIMIT: i64 = 8;
const MAX_LIMIT: i64 = 20;

#[derive(Debug, Deserialize)]
pub struct SuggestQuery {
    /// Préfixe à compléter (peut être vide → aucune suggestion).
    #[serde(default)]
    q: String,
    #[serde(default = "default_limit")]
    limit: i64,
}
fn default_limit() -> i64 {
    DEFAULT_LIMIT
}

#[derive(Debug, Serialize)]
pub struct Suggestions {
    pub suggestions: Vec<String>,
}

pub fn routes(state: SuggestState) -> Router {
    Router::new()
        .route("/search/suggest", get(suggest))
        .with_state(state)
}

const TENANT: Uuid = Uuid::nil();

async fn suggest(
    State(st): State<SuggestState>,
    Query(q): Query<SuggestQuery>,
) -> Result<Json<Suggestions>, (StatusCode, Json<Value>)> {
    let prefix = q.q.trim();
    // Préfixe vide → réponse vide (évite de lister tout le catalogue).
    if prefix.is_empty() {
        return Ok(Json(Suggestions { suggestions: Vec::new() }));
    }
    let limit = q.limit.clamp(1, MAX_LIMIT);
    let suggestions = st
        .db
        .suggest_titles(TENANT, prefix, limit)
        .await
        .map_err(internal)?;
    Ok(Json(Suggestions { suggestions }))
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
