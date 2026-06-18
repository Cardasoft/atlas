//! Orchestration d'ingestion — partie **pure** (doc 26), testée d'abord (TDD).
//! `prepare` calcule tout ce qui ne touche pas à la base : empreinte de contenu,
//! texte indexable, embedding. La persistance (asset/search_text/embedding) est faite
//! ensuite par la couche DB. Séparer le pur de l'I/O rend la logique testable sans infra.

use crate::hash::{average_hash, sha256_hex};
use crate::provenance;
use atlas_embed::Embedder;
use atlas_types::Provenance;

/// Entrée d'ingestion (M1 : métadonnées + texte ; le binaire média viendra ensuite).
pub struct IngestInput<'a> {
    pub title: &'a str,
    pub mime: &'a str,
    /// Texte associé (description, OCR, transcription…) à indexer (FTS).
    pub text: &'a str,
    /// Octets du contenu (pour l'empreinte exacte). Vide si non fourni.
    pub bytes: &'a [u8],
    /// Luminance 8×8 optionnelle pour l'empreinte perceptuelle (images).
    pub luma_8x8: Option<&'a [u8]>,
    /// Origine IA **déclarée** par l'éditeur (ex. champ d'upload), prioritaire sur la
    /// détection par marqueurs. `None` → on s'en remet aux marqueurs embarqués (AI Act §50).
    pub declared_provenance: Option<&'a str>,
    /// Outil/modèle générateur déclaré, si connu.
    pub generator: Option<&'a str>,
}

/// Résultat préparé, prêt à persister.
#[derive(Debug, Clone, PartialEq)]
pub struct PreparedAsset {
    pub content_sha256: String,
    pub phash: u64,
    pub search_text: String,
    pub embedding: Vec<f32>,
    pub status: &'static str,
    /// Provenance / transparence IA (AI Act art. 50, Content Credentials C2PA).
    pub provenance: Provenance,
}

/// Construit l'enregistrement à partir des entrées et de l'encodeur (in-process).
pub fn prepare(input: &IngestInput, embedder: &dyn Embedder) -> PreparedAsset {
    let content_sha256 = sha256_hex(input.bytes);
    let phash = input.luma_8x8.map(average_hash).unwrap_or(0);
    let search_text = compose(&[input.title, input.text]);
    // L'embedding multimodal est calculé sur le texte indexable (M1 ; image plus tard).
    let embedding = embedder.encode(&search_text);
    // Provenance : déclaration éditoriale prioritaire, sinon marqueurs C2PA/IPTC des octets.
    let provenance = provenance::derive(input.bytes, input.declared_provenance, input.generator);
    PreparedAsset {
        content_sha256,
        phash,
        search_text,
        embedding,
        status: "READY",
        provenance,
    }
}

fn compose(parts: &[&str]) -> String {
    parts
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hash::sha256_hex;
    use atlas_embed::FakeEmbedder;

    fn input<'a>(title: &'a str, text: &'a str, bytes: &'a [u8]) -> IngestInput<'a> {
        IngestInput {
            title,
            mime: "image/jpeg",
            text,
            bytes,
            luma_8x8: None,
            declared_provenance: None,
            generator: None,
        }
    }

    #[test]
    fn content_hash_matches_sha256() {
        let p = prepare(&input("Plage", "mer", b"binary-bytes"), &FakeEmbedder);
        assert_eq!(p.content_sha256, sha256_hex(b"binary-bytes"));
    }

    #[test]
    fn search_text_is_composed_from_title_and_text() {
        let p = prepare(&input("Plage été", "coucher de soleil", b""), &FakeEmbedder);
        assert_eq!(p.search_text, "Plage été coucher de soleil");
    }

    #[test]
    fn embedding_has_model_dimension() {
        let e = FakeEmbedder;
        let p = prepare(&input("x", "y", b""), &e);
        assert_eq!(p.embedding.len(), e.dim());
    }

    #[test]
    fn status_is_ready_after_prepare() {
        let p = prepare(&input("x", "y", b""), &FakeEmbedder);
        assert_eq!(p.status, "READY");
    }

    #[test]
    fn deterministic_for_same_input() {
        let a = prepare(&input("x", "y", b"z"), &FakeEmbedder);
        let b = prepare(&input("x", "y", b"z"), &FakeEmbedder);
        assert_eq!(a, b);
    }

    #[test]
    fn provenance_defaults_to_unknown() {
        let p = prepare(&input("x", "y", b"photo"), &FakeEmbedder);
        assert_eq!(p.provenance.ai, atlas_types::AiProvenance::Unknown);
        assert!(!p.provenance.c2pa_present);
    }

    #[test]
    fn declared_provenance_is_recorded() {
        let mut inp = input("x", "y", b"raw");
        inp.declared_provenance = Some("ai-generated");
        inp.generator = Some("Firefly");
        let p = prepare(&inp, &FakeEmbedder);
        assert_eq!(p.provenance.ai, atlas_types::AiProvenance::AiGenerated);
        assert_eq!(p.provenance.generator.as_deref(), Some("Firefly"));
        assert!(p.provenance.transparency_label().is_some());
    }
}
