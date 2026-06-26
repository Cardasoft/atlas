//! Client API du front — `POST /v1/search` via `fetch` (web-sys), même origine.
//!
//! Aucune dépendance HTTP lourde : on passe par l'API `fetch` du navigateur. En dev,
//! le proxy trunk relaie `/v1/*` vers `atlas-core` (:8080) → même origine côté navigateur.

use serde::Serialize;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{Request, RequestInit, RequestMode, Response};

use crate::{SearchResponse, StructuredFilter};

#[derive(Serialize)]
struct SearchRequest<'a> {
    query: &'a str,
    page_size: u32,
    /// Filtres explicites (facettes cliquées) : priment sur les filtres déduits (contrat
    /// `interpreted_query` éditable, doc 25 §4.1). Omis quand aucun filtre n'est actif.
    #[serde(skip_serializing_if = "Option::is_none")]
    filters: Option<&'a StructuredFilter>,
}

/// Appelle `POST /v1/search` et renvoie la réponse désérialisée (ou un message d'erreur).
/// `filters` porte les filtres explicites issus des facettes cliquées (`None` si aucun).
pub async fn search(
    query: &str,
    page_size: u32,
    filters: Option<&StructuredFilter>,
) -> Result<SearchResponse, String> {
    let body = serde_json::to_string(&SearchRequest {
        query,
        page_size,
        filters,
    })
    .map_err(|e| e.to_string())?;

    let opts = RequestInit::new();
    opts.set_method("POST");
    opts.set_mode(RequestMode::SameOrigin);
    opts.set_body(&JsValue::from_str(&body));

    let request = Request::new_with_str_and_init("/v1/search", &opts)
        .map_err(|e| format!("requête : {e:?}"))?;
    request
        .headers()
        .set("Content-Type", "application/json")
        .map_err(|e| format!("en-tête : {e:?}"))?;

    let window = web_sys::window().ok_or("pas de contexte navigateur")?;
    let resp_value = JsFuture::from(window.fetch_with_request(&request))
        .await
        .map_err(|e| format!("réseau : {e:?}"))?;
    let resp: Response = resp_value
        .dyn_into()
        .map_err(|_| "réponse invalide".to_string())?;

    if !resp.ok() {
        return Err(format!("HTTP {}", resp.status()));
    }

    let text_promise = resp.text().map_err(|e| format!("corps : {e:?}"))?;
    let text_value = JsFuture::from(text_promise)
        .await
        .map_err(|e| format!("lecture : {e:?}"))?;
    let text = text_value.as_string().ok_or("corps non textuel")?;

    serde_json::from_str::<SearchResponse>(&text).map_err(|e| format!("JSON : {e}"))
}
