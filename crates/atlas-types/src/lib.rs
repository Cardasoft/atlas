//! Atlas DAM — types partagés (back Rust ↔ front WASM).
//! Conformément à l'API-first (doc 21/22), ces DTO dérivent du contrat OpenAPI.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Statut de cycle de vie d'un asset (préconfiguration par défaut, doc 03 §3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum AssetStatus {
    Uploading,
    Ingesting,
    Ready,
    InReview,
    Approved,
    Published,
    Archived,
    Expired,
}

/// État des droits applicables à un asset (doc 07).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RightsStatus {
    Valid,
    Expiring,
    Expired,
    None,
}

/// Origine d'un contenu au regard de l'IA générative (transparence).
///
/// Aligné sur le règlement européen **AI Act, article 50** (obligations de transparence
/// des contenus générés/manipulés par IA, applicables au 2 août 2026) et sur le vocabulaire
/// **IPTC `digitalSourceType`** repris par les *Content Credentials* **C2PA** :
/// - `trainedAlgorithmicMedia` → contenu **généré** par IA → [`AiProvenance::AiGenerated`] ;
/// - `compositeWithTrainedAlgorithmicMedia`/`algorithmicMedia` → contenu **retouché** par IA
///   → [`AiProvenance::AiEdited`].
///
/// `Human` = pas d'indice d'IA ; `Unknown` = indéterminé (valeur par défaut, sans affirmation).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AiProvenance {
    Human,
    AiGenerated,
    AiEdited,
    /// Indéterminé : valeur par défaut, n'affirme rien (ni IA ni humain).
    #[default]
    Unknown,
}

impl AiProvenance {
    /// Jeton stable utilisé en base et en facette (`ai_provenance`).
    pub fn as_str(&self) -> &'static str {
        match self {
            AiProvenance::Human => "human",
            AiProvenance::AiGenerated => "ai_generated",
            AiProvenance::AiEdited => "ai_edited",
            AiProvenance::Unknown => "unknown",
        }
    }

    /// Lit un jeton (insensible à la casse, tolère tirets/espaces). Inconnu → `Unknown`.
    pub fn from_token(s: &str) -> Self {
        match s
            .trim()
            .to_ascii_lowercase()
            .replace([' ', '-'], "_")
            .as_str()
        {
            "human" | "humain" | "photo" | "real" => AiProvenance::Human,
            "ai_generated" | "ai" | "generated" | "genai" | "synthetic" => {
                AiProvenance::AiGenerated
            }
            "ai_edited" | "edited" | "retouched" | "composite" => AiProvenance::AiEdited,
            _ => AiProvenance::Unknown,
        }
    }

    /// Vrai si le contenu doit porter un libellé de transparence (AI Act art. 50).
    pub fn requires_label(&self) -> bool {
        matches!(self, AiProvenance::AiGenerated | AiProvenance::AiEdited)
    }
}

/// Provenance d'un asset : transparence IA + présence de *Content Credentials* C2PA.
///
/// Persistée à l'ingestion, exposée dans l'asset, en facette de recherche et via l'API,
/// pour répondre aux obligations de marquage/étiquetage de l'AI Act (art. 50).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Provenance {
    /// Origine IA déclarée/détectée (défaut : `Unknown`).
    #[serde(default)]
    pub ai: AiProvenance,
    /// Un manifeste C2PA (*Content Credentials*) signé est présent dans le binaire.
    #[serde(default)]
    pub c2pa_present: bool,
    /// Outil/modèle générateur si connu (ex. « Firefly », « Midjourney »).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generator: Option<String>,
}

impl Provenance {
    /// Libellé de transparence prêt à afficher, ou `None` si aucun n'est requis.
    pub fn transparency_label(&self) -> Option<&'static str> {
        match self.ai {
            AiProvenance::AiGenerated => Some("Contenu généré par IA"),
            AiProvenance::AiEdited => Some("Contenu modifié par IA"),
            _ => None,
        }
    }
}

/// Représentation minimale d'un asset (M0). S'enrichira au fil des fiches.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Asset {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub title: Option<String>,
    pub mime: Option<String>,
    pub status: AssetStatus,
    pub rights_status: RightsStatus,
    /// Provenance / transparence IA (AI Act art. 50). Défaut : indéterminée.
    #[serde(default)]
    pub provenance: Provenance,
}

/// Requête de recherche (doc 04/25) — squelette M0.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchRequest {
    pub query: String,
    #[serde(default = "SearchRequest::default_mode")]
    pub mode: String,
    #[serde(default = "SearchRequest::default_page_size")]
    pub page_size: u32,
    #[serde(default)]
    pub cursor: Option<String>,
}

impl SearchRequest {
    fn default_mode() -> String {
        "natural".to_string()
    }
    fn default_page_size() -> u32 {
        50
    }
}

/// Enveloppe d'événement temps réel poussé par la Realtime Gateway (doc 40).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RealtimeEvent {
    pub op: String,      // "event" | "ack" | "pong" | "resync"
    pub channel: String, // ex. "asset:{id}"
    #[serde(rename = "type")]
    pub kind: String, // ex. "asset.ready"
    pub data: serde_json::Value,
    pub seq: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asset_status_roundtrip() {
        let s = AssetStatus::Ready;
        let j = serde_json::to_string(&s).unwrap();
        assert_eq!(j, "\"READY\"");
        let back: AssetStatus = serde_json::from_str(&j).unwrap();
        assert_eq!(back, AssetStatus::Ready);
    }

    #[test]
    fn search_defaults() {
        let r: SearchRequest = serde_json::from_str(r#"{"query":"plage"}"#).unwrap();
        assert_eq!(r.mode, "natural");
        assert_eq!(r.page_size, 50);
    }

    #[test]
    fn ai_provenance_token_roundtrip() {
        for p in [
            AiProvenance::Human,
            AiProvenance::AiGenerated,
            AiProvenance::AiEdited,
            AiProvenance::Unknown,
        ] {
            assert_eq!(AiProvenance::from_token(p.as_str()), p);
        }
    }

    #[test]
    fn ai_provenance_from_token_is_lenient() {
        assert_eq!(
            AiProvenance::from_token("AI-Generated"),
            AiProvenance::AiGenerated
        );
        assert_eq!(
            AiProvenance::from_token(" GenAI "),
            AiProvenance::AiGenerated
        );
        assert_eq!(AiProvenance::from_token("photo"), AiProvenance::Human);
        assert_eq!(
            AiProvenance::from_token("n'importe quoi"),
            AiProvenance::Unknown
        );
    }

    #[test]
    fn only_ai_content_requires_label() {
        assert!(AiProvenance::AiGenerated.requires_label());
        assert!(AiProvenance::AiEdited.requires_label());
        assert!(!AiProvenance::Human.requires_label());
        assert!(!AiProvenance::Unknown.requires_label());
    }

    #[test]
    fn provenance_serialises_with_snake_case_tokens() {
        let p = Provenance {
            ai: AiProvenance::AiGenerated,
            c2pa_present: true,
            generator: Some("Firefly".into()),
        };
        let j = serde_json::to_string(&p).unwrap();
        assert!(
            j.contains("\"ai_generated\""),
            "jeton snake_case attendu : {j}"
        );
        let back: Provenance = serde_json::from_str(&j).unwrap();
        assert_eq!(back, p);
    }

    #[test]
    fn provenance_default_is_unknown_without_label() {
        let p = Provenance::default();
        assert_eq!(p.ai, AiProvenance::Unknown);
        assert!(!p.c2pa_present);
        assert_eq!(p.transparency_label(), None);
    }
}
