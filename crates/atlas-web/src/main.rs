//! Atlas DAM — front WASM (Leptos CSR), M1 « démarrage ».
//!
//! Brique du front (le produit pour les utilisateurs métier, jusqu'ici absent) : une
//! **recherche d'assets** qui appelle le contrat d'API `POST /v1/search` (doc 25) et
//! affiche les résultats. Souverain/frugal : l'appel réseau passe par `fetch` (web-sys),
//! même origine via le proxy trunk en dev — aucun SDK/CDN, aucune dépendance lourde.
//!
//! Au-delà de la liste titre/score, l'UI surface deux valeurs cœur de DAM directement
//! issues du contrat existant (aucun changement back) : le **statut de droits**
//! (`rights_status`) par résultat — la gestion des droits est centrale en DAM — et la
//! **compréhension de requête** (`interpreted_query` : texte sémantique, filtres déduits,
//! confiance) — différenciateur d'Atlas (doc 25 §4.1, filtres éditables) : on montre à
//! l'utilisateur ce que le moteur a compris. La grille/visionneuse/badge provenance IA
//! viendront aux jalons suivants en réutilisant ce socle + le client API `api::search`.

use leptos::prelude::*;
use serde::{Deserialize, Serialize};

mod api;

/// Un résultat (`SearchResultItem` OpenAPI) — champs optionnels pour rester tolérant
/// à l'évolution du contrat (un champ absent ne casse pas le front).
#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct SearchResultItem {
    pub asset_id: Option<String>,
    pub title: Option<String>,
    pub score: Option<f64>,
    /// Statut de droits d'usage (gestion des droits = valeur cœur DAM) ; omis par le serveur si inconnu.
    pub rights_status: Option<String>,
}

/// Filtres structurés déduits/explicites (`StructuredFilter` OpenAPI) — tous optionnels.
/// `r#type` (sérialisé `"type"` côté serveur) liste les types d'asset retenus.
#[derive(Deserialize, Serialize, Clone, Debug, Default)]
pub struct StructuredFilter {
    pub has_people: Option<bool>,
    pub orientation: Option<String>,
    pub rights_status: Option<String>,
    #[serde(default)]
    pub r#type: Vec<String>,
}

/// Compréhension de requête (`InterpretedQuery` OpenAPI) : ce que le moteur a compris de
/// la saisie libre. Affichée pour la transparence (différenciateur « query understanding »).
#[derive(Deserialize, Serialize, Clone, Debug, Default)]
pub struct InterpretedQuery {
    #[serde(default)]
    pub semantic_text: String,
    #[serde(default)]
    pub filters: StructuredFilter,
    #[serde(default)]
    pub confidence: f32,
    #[serde(default)]
    pub editable: bool,
}

/// Réponse de recherche (`SearchResponse` OpenAPI) — sous-ensemble affiché.
#[derive(Deserialize, Serialize, Clone, Debug, Default)]
pub struct SearchResponse {
    #[serde(default)]
    pub results: Vec<SearchResultItem>,
    #[serde(default)]
    pub interpreted_query: InterpretedQuery,
    #[serde(default)]
    pub degraded: bool,
}

impl StructuredFilter {
    /// Étiquettes lisibles des filtres déduits, pour les afficher en « chips ». Vide si
    /// aucun filtre déduit (l'UI masque alors la ligne).
    fn chips(&self) -> Vec<String> {
        let mut out = Vec::new();
        match self.has_people {
            Some(true) => out.push("avec personnes".to_string()),
            Some(false) => out.push("sans personne".to_string()),
            None => {}
        }
        if let Some(o) = &self.orientation {
            out.push(format!("orientation : {o}"));
        }
        if let Some(r) = &self.rights_status {
            out.push(format!("droits : {r}"));
        }
        for t in &self.r#type {
            out.push(format!("type : {t}"));
        }
        out
    }
}

/// Classe CSS d'un badge de droits selon une heuristique sur le libellé (vert = libre,
/// orange = restreint/refusé, neutre sinon). Cosmétique seulement — la source de vérité
/// reste le serveur.
fn rights_class(status: &str) -> &'static str {
    let s = status.to_ascii_lowercase();
    if s.contains("restrict") || s.contains("denied") || s.contains("refus") || s.contains("expir")
    {
        "badge warn"
    } else if s.contains("clear")
        || s.contains("approv")
        || s.contains("libre")
        || s.contains("public")
    {
        "badge ok"
    } else {
        "badge"
    }
}

fn main() {
    leptos::mount::mount_to_body(App);
}

#[component]
fn App() -> impl IntoView {
    let query = RwSignal::new(String::new());
    let results = RwSignal::new(Vec::<SearchResultItem>::new());
    let status = RwSignal::new(String::from("Prêt."));
    // Compréhension de la dernière recherche aboutie (None tant qu'aucune n'a répondu).
    let interpreted = RwSignal::new(None::<InterpretedQuery>);
    let degraded = RwSignal::new(false);
    // `true` dès qu'une recherche a répondu : distingue « pas encore cherché » de « 0 résultat ».
    let searched = RwSignal::new(false);

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
                    interpreted.set(Some(sr.interpreted_query));
                    degraded.set(sr.degraded);
                    results.set(sr.results);
                    searched.set(true);
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

            // Bandeau « mode dégradé » (ex. vectoriel indisponible) — transparence sur la qualité.
            {move || {
                degraded
                    .get()
                    .then(|| {
                        view! {
                            <p class="degraded">
                                "⚠ Mode dégradé — résultats partiels (recherche sémantique indisponible)."
                            </p>
                        }
                    })
            }}

            // Compréhension de requête : ce que le moteur a compris (texte sémantique,
            // confiance, filtres déduits). Différenciateur « query understanding » d'Atlas.
            {move || {
                interpreted
                    .get()
                    .map(|iq| {
                        let conf = format!("{:.0} %", (iq.confidence.clamp(0.0, 1.0)) * 100.0);
                        let chips = iq.filters.chips();
                        let sem = iq.semantic_text.clone();
                        view! {
                            <div class="understand">
                                <span class="u-label">"Compris"</span>
                                {(!sem.is_empty())
                                    .then(|| view! { <span class="u-sem">"« " {sem} " »"</span> })}
                                <span class="u-conf">"confiance " {conf}</span>
                                {(!chips.is_empty())
                                    .then(|| {
                                        view! {
                                            <span class="chips">
                                                {chips
                                                    .into_iter()
                                                    .map(|c| view! { <span class="chip">{c}</span> })
                                                    .collect_view()}
                                            </span>
                                        }
                                    })}
                            </div>
                        }
                    })
            }}

            <ul class="results">
                <For
                    each=move || results.get()
                    key=|r| r.asset_id.clone().unwrap_or_default()
                    children=move |r: SearchResultItem| {
                        let titre = r.title.clone().unwrap_or_else(|| "(sans titre)".to_string());
                        let score = r.score.unwrap_or(0.0);
                        let rights = r.rights_status.clone();
                        view! {
                            <li>
                                <span class="title">{titre}</span>
                                <span class="meta">
                                    {rights
                                        .map(|st| {
                                            let cls = rights_class(&st);
                                            view! { <span class=cls>"droits : " {st}</span> }
                                        })}
                                    <span class="score">{format!("{score:.3}")}</span>
                                </span>
                            </li>
                        }
                    }
                />
            </ul>

            // État vide explicite : une recherche a répondu mais 0 résultat (≠ « pas encore cherché »).
            {move || {
                (searched.get() && results.get().is_empty())
                    .then(|| view! { <p class="empty">"Aucun asset ne correspond à cette recherche."</p> })
            }}
        </main>
    }
}
