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

/// Représentation minimale d'un asset (M0). S'enrichira au fil des fiches.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Asset {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub title: Option<String>,
    pub mime: Option<String>,
    pub status: AssetStatus,
    pub rights_status: RightsStatus,
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
}
