//! Query understanding par règles (doc 25 §4.1).
//! Couche déterministe rapide (< 10 ms) ; le LLM léger n'intervient qu'en cas d'ambiguïté
//! (non implémenté dans ce squelette M1).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct StructuredFilter {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub has_people: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub orientation: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rights_status: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub r#type: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InterpretedQuery {
    pub semantic_text: String,
    pub filters: StructuredFilter,
    pub confidence: f32,
    pub editable: bool,
}

/// Analyse une requête en langage naturel (FR/EN) par règles.
/// Extrait des filtres implicites ; le texte résiduel sert au sémantique/lexical.
pub fn interpret(query: &str) -> InterpretedQuery {
    let q = query.to_lowercase();
    let mut f = StructuredFilter::default();

    if q.contains("sans personne")
        || q.contains("sans monde")
        || q.contains("no people")
        || q.contains("without people")
    {
        f.has_people = Some(false);
    }
    if q.contains("paysage") || q.contains("landscape") {
        f.orientation = Some("landscape".into());
    } else if q.contains("portrait") {
        f.orientation = Some("portrait".into());
    }
    if q.contains("libre de droit")
        || q.contains("libres de droits")
        || q.contains("droits valides")
        || q.contains("royalty free")
    {
        f.rights_status = Some("valid".into());
    }
    if q.contains("vidéo") || q.contains("video") {
        f.r#type.push("video".into());
    }

    InterpretedQuery {
        semantic_text: query.trim().to_string(),
        filters: f,
        confidence: 0.9, // règles seules : confiance élevée et déterministe
        editable: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_landscape_no_people_rights() {
        let i =
            interpret("plage au coucher de soleil sans personne, format paysage, libre de droit");
        assert_eq!(i.filters.has_people, Some(false));
        assert_eq!(i.filters.orientation.as_deref(), Some("landscape"));
        assert_eq!(i.filters.rights_status.as_deref(), Some("valid"));
    }

    #[test]
    fn extracts_video_type_en() {
        let i = interpret("brand video without people");
        assert_eq!(i.filters.has_people, Some(false));
        assert_eq!(i.filters.r#type, vec!["video".to_string()]);
    }

    #[test]
    fn plain_query_has_no_filters() {
        let i = interpret("mer");
        assert_eq!(i.filters, StructuredFilter::default());
    }
}
