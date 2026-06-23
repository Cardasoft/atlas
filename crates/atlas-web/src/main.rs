//! Atlas DAM — front WASM (Leptos CSR), M1 « démarrage ».
//!
//! Première brique du front (le produit pour les utilisateurs métier, jusqu'ici absent) :
//! une **recherche d'assets** qui appelle le contrat d'API `POST /v1/search` (doc 25) et
//! affiche les résultats. Souverain/frugal : l'appel réseau passe par `fetch` (web-sys),
//! même origine via le proxy trunk en dev — aucun SDK/CDN, aucune dépendance lourde.
//! Le reste de l'UI (grille, facettes, visionneuse, badge provenance IA) viendra aux
//! jalons suivants en réutilisant ce socle + le client API `api::search`.

use leptos::prelude::*;
use serde::{Deserialize, Serialize};

mod api;

/// Un résultat (`SearchResultItem` OpenAPI) — champs optionnels pour rester tolérant.
#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct SearchResultItem {
    pub asset_id: Option<String>,
    pub title: Option<String>,
    pub score: Option<f64>,
}

/// Réponse de recherche (`SearchResponse` OpenAPI) — sous-ensemble affiché.
#[derive(Deserialize, Serialize, Clone, Debug, Default)]
pub struct SearchResponse {
    #[serde(default)]
    pub results: Vec<SearchResultItem>,
    #[serde(default)]
    pub degraded: bool,
}

fn main() {
    leptos::mount::mount_to_body(App);
}

#[component]
fn App() -> impl IntoView {
    let query = RwSignal::new(String::new());
    let results = RwSignal::new(Vec::<SearchResultItem>::new());
    let status = RwSignal::new(String::from("Prêt."));

    // Lance une recherche via le client API (POST /v1/search).
    let lancer = move || {
        let q = query.get();
        if q.trim().is_empty() {
            status.set("Saisis une requête.".to_string());
            return;
        }
        status.set("Recherche…".to_string());
        wasm_bindgen_futures::spawn_local(async move {
            match api::search(&q, 20).await {
                Ok(sr) => {
                    let n = sr.results.len();
                    let suffix = if sr.degraded { " (mode dégradé)" } else { "" };
                    results.set(sr.results);
                    status.set(format!("{n} résultat(s){suffix}"));
                }
                Err(e) => status.set(format!("Erreur : {e}")),
            }
        });
    };

    view! {
        <main class="wrap">
            <h1>"Atlas " <span class="dim">"DAM"</span></h1>
            <p class="sub">"Recherche d'assets — souveraine, IA-first"</p>
            <div class="row">
                <input
                    class="q"
                    type="text"
                    placeholder="Rechercher…  (ex. plage paysage sans personne)"
                    prop:value=move || query.get()
                    on:input=move |ev| query.set(event_target_value(&ev))
                    on:keydown=move |ev| {
                        if ev.key() == "Enter" {
                            lancer();
                        }
                    }
                />
                <button class="go" on:click=move |_| lancer()>
                    "Rechercher"
                </button>
            </div>
            <p class="status">{move || status.get()}</p>
            <ul class="results">
                <For
                    each=move || results.get()
                    key=|r| r.asset_id.clone().unwrap_or_default()
                    children=move |r: SearchResultItem| {
                        let titre = r.title.clone().unwrap_or_else(|| "(sans titre)".to_string());
                        let score = r.score.unwrap_or(0.0);
                        view! {
                            <li>
                                <span class="title">{titre}</span>
                                <span class="score">{format!("{score:.3}")}</span>
                            </li>
                        }
                    }
                />
            </ul>
        </main>
    }
}
