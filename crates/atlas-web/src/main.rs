//! Atlas DAM — front WASM (Leptos CSR), M1 « démarrage ».
//!
//! Brique du front (le produit pour les utilisateurs métier, jusqu'ici absent) : une
//! **recherche d'assets** qui appelle le contrat d'API `POST /v1/search` (doc 25) et
//! affiche les résultats. Souverain/frugal : l'appel réseau passe par `fetch` (web-sys),
//! même origine via le proxy trunk en dev — aucun SDK/CDN, aucune dépendance lourde.
//!
//! Au-delà de la liste titre/score, l'UI surface plusieurs valeurs cœur de DAM, toutes
//! issues du contrat existant (aucun changement back) : le **statut de droits**
//! (`rights_status`) par résultat — la gestion des droits est centrale en DAM —, la
//! **compréhension de requête** (`interpreted_query`) — différenciateur d'Atlas (doc 25
//! §4.1, filtres éditables) —, et désormais les **facettes** (`facets`, doc 25 §4.5) :
//! agrégations par dimension (orientation, droits, format, **provenance IA** — AI Act
//! art. 50 —, personnes). Les facettes mappables sur un filtre structuré sont **cliquables
//! pour raffiner** la recherche (filtres explicites renvoyés au serveur, qui priment sur
//! les filtres déduits) ; les autres (format, provenance IA) sont affichées en lecture
//! seule. La grille/visionneuse viendront aux jalons suivants en réutilisant ce socle.

use leptos::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

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

/// Filtres structurés déduits/explicites (`StructuredFilter` OpenAPI). Sert à la fois à
/// **désérialiser** les filtres déduits (`interpreted_query.filters`) et à **sérialiser**
/// les filtres explicites renvoyés au serveur (facettes cliquées) — d'où les
/// `skip_serializing_if` qui alignent l'émission sur le contrat (champs absents = non posés).
/// `r#type` (sérialisé `"type"` côté serveur) liste les types d'asset retenus.
#[derive(Deserialize, Serialize, Clone, Debug, Default, PartialEq)]
pub struct StructuredFilter {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub has_people: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub orientation: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rights_status: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
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

/// Comptage d'une valeur de facette (`FacetCount` OpenAPI, doc 25 §4.5). Champs tolérants.
#[derive(Deserialize, Serialize, Clone, Debug, Default, PartialEq)]
pub struct FacetCount {
    #[serde(default)]
    pub value: String,
    #[serde(default)]
    pub count: u64,
}

/// Réponse de recherche (`SearchResponse` OpenAPI) — sous-ensemble affiché.
#[derive(Deserialize, Serialize, Clone, Debug, Default)]
pub struct SearchResponse {
    #[serde(default)]
    pub results: Vec<SearchResultItem>,
    #[serde(default)]
    pub interpreted_query: InterpretedQuery,
    /// Agrégations par facette (doc 25 §4.5) : nom de facette → valeurs ordonnées. Vide si omis.
    #[serde(default)]
    pub facets: BTreeMap<String, Vec<FacetCount>>,
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

    /// Vrai si aucun filtre explicite n'est posé (→ on n'envoie pas `filters` au serveur).
    fn is_empty(&self) -> bool {
        self.has_people.is_none()
            && self.orientation.is_none()
            && self.rights_status.is_none()
            && self.r#type.is_empty()
    }

    /// La valeur d'une facette filtrable est-elle actuellement active ?
    fn is_active(&self, key: &str, value: &str) -> bool {
        match key {
            "orientation" => self.orientation.as_deref() == Some(value),
            "rights_status" => self.rights_status.as_deref() == Some(value),
            "has_people" => self.has_people == parse_bool(value),
            _ => false,
        }
    }

    /// Copie où la valeur de la facette est **basculée** (activée si absente, retirée si
    /// déjà active). Champs mono-valeur : une nouvelle valeur remplace l'ancienne.
    fn toggled(&self, key: &str, value: &str) -> StructuredFilter {
        let mut f = self.clone();
        match key {
            "orientation" => f.orientation = toggle_opt(f.orientation, value),
            "rights_status" => f.rights_status = toggle_opt(f.rights_status, value),
            "has_people" => {
                let b = parse_bool(value);
                f.has_people = if f.has_people == b { None } else { b };
            }
            _ => {}
        }
        f
    }

    /// Filtres explicites actifs sous forme `(libellé, clé, valeur)` — pour la barre de
    /// filtres actifs (chaque chip se retire en re-basculant `(clé, valeur)`).
    fn active_filters(&self) -> Vec<(String, &'static str, String)> {
        let mut out = Vec::new();
        if let Some(b) = self.has_people {
            let val = if b { "true" } else { "false" };
            out.push((
                facet_value_label("has_people", val),
                "has_people",
                val.to_string(),
            ));
        }
        if let Some(o) = &self.orientation {
            out.push((format!("orientation : {o}"), "orientation", o.clone()));
        }
        if let Some(r) = &self.rights_status {
            out.push((format!("droits : {r}"), "rights_status", r.clone()));
        }
        out
    }
}

/// Parse une valeur booléenne de facette (`"true"`/`"false"`), `None` sinon.
fn parse_bool(v: &str) -> Option<bool> {
    match v {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

/// Bascule un champ `Option<String>` mono-valeur : retire si déjà cette valeur, sinon la pose.
fn toggle_opt(cur: Option<String>, value: &str) -> Option<String> {
    if cur.as_deref() == Some(value) {
        None
    } else {
        Some(value.to_string())
    }
}

/// Une facette est-elle **cliquable** (= mappable sur un champ de `StructuredFilter`) ?
/// `mime`/`ai_provenance` n'ont pas de champ de filtre dédié → affichées en lecture seule.
fn facet_is_filterable(key: &str) -> bool {
    matches!(key, "orientation" | "rights_status" | "has_people")
}

/// Libellé FR de la dimension de facette (clé du serveur → titre lisible).
fn facet_label(key: &str) -> String {
    match key {
        "orientation" => "Orientation",
        "rights_status" => "Droits",
        "mime" => "Format",
        "ai_provenance" => "Provenance IA",
        "has_people" => "Personnes",
        other => other,
    }
    .to_string()
}

/// Libellé FR d'une valeur de facette (humanise les booléens `has_people`).
fn facet_value_label(key: &str, value: &str) -> String {
    match (key, value) {
        ("has_people", "true") => "avec personnes".to_string(),
        ("has_people", "false") => "sans personne".to_string(),
        _ => value.to_string(),
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
    // Facettes (agrégations) de la dernière recherche aboutie.
    let facets = RwSignal::new(BTreeMap::<String, Vec<FacetCount>>::new());
    // Filtres explicites actifs (posés en cliquant les facettes) ; renvoyés au serveur.
    let filters = RwSignal::new(StructuredFilter::default());
    let degraded = RwSignal::new(false);
    // `true` dès qu'une recherche a répondu : distingue « pas encore cherché » de « 0 résultat ».
    let searched = RwSignal::new(false);

    // Lance une recherche via le client API (POST /v1/search), en transmettant les filtres
    // explicites actifs (facettes cliquées). Copy (ne capture que des signaux Copy) → réutilisable.
    let lancer = move || {
        let q = query.get();
        if q.trim().is_empty() {
            status.set("Saisis une requête.".to_string());
            return;
        }
        status.set("Recherche…".to_string());
        let f = filters.get();
        let f_opt = if f.is_empty() { None } else { Some(f) };
        wasm_bindgen_futures::spawn_local(async move {
            match api::search(&q, 20, f_opt.as_ref()).await {
                Ok(sr) => {
                    let n = sr.results.len();
                    let suffix = if sr.degraded { " (mode dégradé)" } else { "" };
                    interpreted.set(Some(sr.interpreted_query));
                    facets.set(sr.facets);
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

            // Filtres explicites actifs (facettes cliquées) : chaque chip se retire, + « Tout effacer ».
            {move || {
                let f = filters.get();
                (!f.is_empty())
                    .then(|| {
                        let chips = f.active_filters();
                        view! {
                            <div class="active-filters">
                                <span class="af-label">"Filtres actifs"</span>
                                {chips
                                    .into_iter()
                                    .map(|(label, key, value)| {
                                        view! {
                                            <button
                                                class="af-chip"
                                                on:click=move |_| {
                                                    filters.update(|fl| *fl = fl.toggled(key, &value));
                                                    lancer();
                                                }
                                            >
                                                {label}
                                                <span class="af-x">"×"</span>
                                            </button>
                                        }
                                    })
                                    .collect_view()}
                                <button
                                    class="af-clear"
                                    on:click=move |_| {
                                        filters.set(StructuredFilter::default());
                                        lancer();
                                    }
                                >
                                    "Tout effacer"
                                </button>
                            </div>
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

            // Facettes (agrégations par dimension, doc 25 §4.5) : orientation, droits, format,
            // provenance IA (AI Act art. 50), personnes. Les facettes mappables sur un filtre
            // sont cliquables pour raffiner ; les autres sont en lecture seule.
            {move || {
                let active = filters.get();
                let groups = facets.get();
                (!groups.is_empty())
                    .then(|| {
                        view! {
                            <section class="facets">
                                {groups
                                    .into_iter()
                                    .map(|(key, counts)| {
                                        let filterable = facet_is_filterable(&key);
                                        let name = facet_label(&key);
                                        let vals = counts
                                            .into_iter()
                                            .map(|fc| {
                                                let label = facet_value_label(&key, &fc.value);
                                                let count = fc.count;
                                                if filterable {
                                                    let on = active.is_active(&key, &fc.value);
                                                    let cls = if on {
                                                        "facet-val active"
                                                    } else {
                                                        "facet-val"
                                                    };
                                                    let k = key.clone();
                                                    let v = fc.value.clone();
                                                    view! {
                                                        <button
                                                            class=cls
                                                            on:click=move |_| {
                                                                filters.update(|f| *f = f.toggled(&k, &v));
                                                                lancer();
                                                            }
                                                        >
                                                            {label}
                                                            <span class="fc">{count}</span>
                                                        </button>
                                                    }
                                                        .into_any()
                                                } else {
                                                    view! {
                                                        <span class="facet-val ro">
                                                            {label}
                                                            <span class="fc">{count}</span>
                                                        </span>
                                                    }
                                                        .into_any()
                                                }
                                            })
                                            .collect_view();
                                        view! {
                                            <div class="facet">
                                                <span class="facet-name">{name}</span>
                                                <span class="facet-vals">{vals}</span>
                                            </div>
                                        }
                                    })
                                    .collect_view()}
                            </section>
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
