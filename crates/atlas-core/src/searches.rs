//! Endpoints des recherches enregistrées (doc 25 §3.2 / doc 22).
//! `POST /v1/searches` (enregistrer), `GET /v1/searches` (lister), `DELETE /v1/searches/{id}`.
//! Disponibles uniquement si PostgreSQL est branché. Le payload `query` est un objet JSON
//! libre (typiquement un corps `/v1/search`) conservé tel quel pour rejeu/édition.

use atlas_search::{AuthCtx, Identity};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::{delete as delete_route, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

#[derive(Clone)]
pub struct SearchesState {
    pub db: atlas_db::Db,
}

#[derive(Debug, Deserialize)]
pub struct CreateSavedSearchRequest {
    pub name: String,
    /// Corps de recherche à rejouer (objet JSON libre, p.ex. un `SearchRequest`).
    pub query: Value,
    #[serde(default)]
    pub notify: bool,
}

#[derive(Debug, Serialize)]
pub struct SavedSearchView {
    pub id: Uuid,
    pub name: String,
    pub query: Value,
    pub notify: bool,
    pub created_at: String,
}

pub fn routes(state: SearchesState) -> Router {
    Router::new()
        .route("/searches", post(create).get(list))
        .route("/searches/:id", delete_route(delete))
        .with_state(state)
}

/// Propriétaire effectif : l'utilisateur résolu, ou nil en mono-utilisateur (dev, doc 38).
fn owner_of(ctx: &AuthCtx) -> Uuid {
    ctx.user_id.unwrap_or_else(Uuid::nil)
}

async fn create(
    State(st): State<SearchesState>,
    Identity(ctx): Identity,
    Json(req): Json<CreateSavedSearchRequest>,
) -> Result<(StatusCode, Json<Value>), (StatusCode, Json<Value>)> {
    let query_json = req.query.to_string();
    let id = st
        .db
        .create_saved_search(
            ctx.tenant_id,
            owner_of(&ctx),
            &req.name,
            &query_json,
            req.notify,
        )
        .await
        .map_err(internal)?;
    Ok((StatusCode::CREATED, Json(json!({ "id": id }))))
}

async fn list(
    State(st): State<SearchesState>,
    Identity(ctx): Identity,
) -> Result<Json<Vec<SavedSearchView>>, (StatusCode, Json<Value>)> {
    let rows = st
        .db
        .list_saved_searches(ctx.tenant_id, owner_of(&ctx))
        .await
        .map_err(internal)?;
    let out = rows
        .into_iter()
        .map(|s| SavedSearchView {
            id: s.id,
            name: s.name,
            // Le texte vient de jsonb (PostgreSQL) → JSON valide ; fallback string par prudence.
            query: serde_json::from_str(&s.query).unwrap_or(Value::String(s.query)),
            notify: s.notify,
            created_at: s.created_at,
        })
        .collect();
    Ok(Json(out))
}

async fn delete(
    State(st): State<SearchesState>,
    Identity(ctx): Identity,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, (StatusCode, Json<Value>)> {
    let deleted = st
        .db
        .delete_saved_search(ctx.tenant_id, owner_of(&ctx), id)
        .await
        .map_err(internal)?;
    if deleted {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err((
            StatusCode::NOT_FOUND,
            Json(json!({
                "type": "https://atlas.local/errors/not-found",
                "title": "Recherche introuvable"
            })),
        ))
    }
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
