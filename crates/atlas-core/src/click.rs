//! Endpoint de capture de clic (doc 25 §4.4/§6) : `POST /v1/search/click`.
//! Disponible uniquement si PostgreSQL est branché. Le client renvoie le `query_hash` reçu
//! dans la réponse de recherche et l'`asset_id` ouvert : le clic est rattaché à la dernière
//! recherche correspondante (RLS) et alimente le signal de popularité. Tenant résolu depuis
//! l'identité (doc 38). Un clic orphelin (hash inconnu) est ignoré sans erreur.

use atlas_search::Identity;
use axum::{extract::State, http::StatusCode, routing::post, Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

#[derive(Clone)]
pub struct ClickState {
    pub db: atlas_db::Db,
}

#[derive(Debug, Deserialize)]
pub struct ClickRequest {
    /// Hash de la requête tel que renvoyé par `POST /v1/search` (`query_hash`).
    pub query_hash: String,
    /// Asset ouvert par l'utilisateur.
    pub asset_id: Uuid,
}

pub fn routes(state: ClickState) -> Router {
    Router::new()
        .route("/search/click", post(record_click))
        .with_state(state)
}

async fn record_click(
    State(st): State<ClickState>,
    Identity(ctx): Identity,
    Json(req): Json<ClickRequest>,
) -> Result<StatusCode, (StatusCode, Json<Value>)> {
    st.db
        .record_click(ctx.tenant_id, &req.query_hash, req.asset_id)
        .await
        .map_err(internal)?;
    // 204 que le clic ait été rattaché ou non (orphelin ignoré) : signal best-effort.
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
