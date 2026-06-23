//! Endpoint d'ingestion `POST /v1/assets` (doc 26 / doc 22).
//! Orchestration M1 : prepare (pur) → persistance (asset, search_text, embedding) →
//! l'asset est immédiatement cherchable. Disponible uniquement si PostgreSQL est branché.

use atlas_embed::Embedder;
use atlas_ingest::prepare::{prepare, IngestInput};
use atlas_search::Identity;
use axum::{extract::State, http::StatusCode, routing::post, Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;

#[derive(Clone)]
pub struct AssetsState {
    pub db: atlas_db::Db,
    pub embedder: Arc<dyn Embedder>,
    pub hub: atlas_realtime::Hub,
    /// Cache de recherche partagé : purgé pour le tenant à chaque ingestion (doc 25 §6).
    pub cache: Arc<dyn atlas_search::cache::SearchCache>,
}

#[derive(Debug, Deserialize)]
pub struct CreateAssetRequest {
    pub title: String,
    #[serde(default = "default_mime")]
    pub mime: String,
    /// Texte à indexer (description/OCR/transcription). Sert aussi à l'empreinte de contenu (M1).
    #[serde(default)]
    pub text: String,
    /// Origine IA **déclarée** par l'éditeur (« human », « ai_generated », « ai_edited »…).
    /// Prime sur la détection par marqueurs C2PA/IPTC (AI Act art. 50, transparence).
    #[serde(default)]
    pub provenance: Option<String>,
    /// Outil/modèle générateur déclaré, si connu (ex. « Firefly »).
    #[serde(default)]
    pub generator: Option<String>,
}
fn default_mime() -> String {
    "application/octet-stream".into()
}

#[derive(Debug, Serialize)]
pub struct CreateAssetResponse {
    pub id: Uuid,
    pub status: String,
    pub content_sha256: String,
    /// Provenance / transparence IA retenue pour l'asset (AI Act art. 50).
    pub provenance: atlas_types::Provenance,
    /// Libellé de transparence à afficher si le contenu relève de l'IA, sinon `null`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transparency_label: Option<&'static str>,
}

pub fn routes(state: AssetsState) -> Router {
    Router::new()
        .route("/assets", post(create_asset))
        .with_state(state)
}

async fn create_asset(
    State(st): State<AssetsState>,
    Identity(ctx): Identity,
    Json(req): Json<CreateAssetRequest>,
) -> Result<(StatusCode, Json<CreateAssetResponse>), (StatusCode, Json<Value>)> {
    let tenant = ctx.tenant_id;

    let input = IngestInput {
        title: &req.title,
        mime: &req.mime,
        text: &req.text,
        bytes: req.text.as_bytes(), // M1 : empreinte sur le texte ; binaire média ensuite.
        luma_8x8: None,
        declared_provenance: req.provenance.as_deref(),
        generator: req.generator.as_deref(),
    };
    let prepared = prepare(&input, st.embedder.as_ref());

    let id = st
        .db
        .insert_asset(
            tenant,
            &req.title,
            &req.mime,
            prepared.status,
            "none",
            None,
            None,
            &prepared.provenance,
        )
        .await
        .map_err(internal)?;
    st.db
        .upsert_search_text(tenant, id, "simple", &prepared.search_text)
        .await
        .map_err(internal)?;
    st.db
        .upsert_embedding(tenant, id, "fake", &prepared.embedding)
        .await
        .map_err(internal)?;

    // Le périmètre du tenant a changé : on purge son cache de recherche pour éviter de servir
    // des résultats périmés qui ignoreraient l'asset fraîchement indexé (doc 25 §6).
    st.cache.invalidate_tenant(tenant).await;

    // Temps réel : notifie les UI abonnées (canaux "ingest" et "asset:{id}"), doc 40.
    // On transmet la provenance pour que l'UI affiche d'emblée le libellé de transparence.
    let label = prepared.provenance.transparency_label();
    let payload = json!({
        "id": id,
        "status": prepared.status,
        "title": req.title,
        "ai_provenance": prepared.provenance.ai.as_str(),
        "c2pa_present": prepared.provenance.c2pa_present,
        "transparency_label": label,
    });
    st.hub.publish("ingest", "asset.ingested", payload.clone());
    st.hub
        .publish(format!("asset:{id}"), "asset.ingested", payload);

    Ok((
        StatusCode::CREATED,
        Json(CreateAssetResponse {
            id,
            status: prepared.status.to_string(),
            content_sha256: prepared.content_sha256,
            transparency_label: label,
            provenance: prepared.provenance,
        }),
    ))
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
