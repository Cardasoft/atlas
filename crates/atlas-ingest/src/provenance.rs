//! Détection de **provenance & transparence IA** à l'ingestion (doc 26).
//!
//! ## Pourquoi
//! Le règlement européen **AI Act (art. 50)** impose, à compter du 2 août 2026, de
//! **marquer et étiqueter** les contenus générés ou manipulés par IA. Le *Code de bonnes
//! pratiques sur la transparence des contenus générés par IA* (Commission européenne,
//! 10 juin 2026) cite explicitement les **Content Credentials C2PA** comme mécanisme de
//! référence. Un DAM est précisément le point où cette provenance doit être **captée puis
//! exposée** (asset, facette, API).
//!
//! ## Logique **pure** (testable sans I/O)
//! Ce module n'effectue aucune E/S : il **inspecte les octets** du contenu pour y repérer
//! des marqueurs standardisés, et combine le résultat avec une éventuelle **déclaration**
//! explicite fournie à l'ingestion. Le parsing/validation cryptographique complet d'un
//! manifeste C2PA signé (crate `c2pa`, dépendances natives) relève d'un jalon ultérieur ;
//! la détection heuristique ici reste volontairement sobre, hermétique et air-gap.

use atlas_types::{AiProvenance, Provenance};

/// Marqueur de boîte **JUMBF** (ISO/IEC 19566-5) qui encapsule un manifeste C2PA.
const JUMBF_BOX: &[u8] = b"jumb";
/// Étiquette du super-boîtier C2PA (`urn:c2pa` / label « c2pa »).
const C2PA_LABEL: &[u8] = b"c2pa";

/// Marqueurs **IPTC `digitalSourceType`** (repris par les assertions C2PA) indiquant que
/// le média a été **entièrement produit** par un algorithme entraîné → généré par IA.
const SRC_TRAINED: &[u8] = b"trainedAlgorithmicMedia";
/// Média **composite** mêlant des éléments produits par IA → modifié/retouché par IA.
const SRC_COMPOSITE: &[u8] = b"compositeWithTrainedAlgorithmicMedia";
/// Média produit par un algorithme non entraîné (rendu paramétrique) → assimilé édition IA.
const SRC_ALGORITHMIC: &[u8] = b"algorithmicMedia";

/// Présence d'un manifeste **C2PA** (Content Credentials) dans le binaire.
///
/// Heuristique : on exige conjointement la boîte JUMBF (`jumb`) **et** l'étiquette `c2pa`,
/// pour éviter les faux positifs sur des fichiers contenant fortuitement « c2pa ».
pub fn detect_c2pa(bytes: &[u8]) -> bool {
    contains(bytes, JUMBF_BOX) && contains(bytes, C2PA_LABEL)
}

/// Déduit l'origine IA à partir des **marqueurs de provenance embarqués** dans les octets
/// (vocabulaire IPTC `digitalSourceType`). Renvoie `None` si aucun marqueur n'est trouvé.
pub fn ai_from_markers(bytes: &[u8]) -> Option<AiProvenance> {
    // L'ordre compte : `compositeWith...` contient `...AlgorithmicMedia`, on teste donc
    // le composite (édition) avant le pur généré.
    if contains(bytes, SRC_COMPOSITE) {
        Some(AiProvenance::AiEdited)
    } else if contains(bytes, SRC_TRAINED) {
        Some(AiProvenance::AiGenerated)
    } else if contains(bytes, SRC_ALGORITHMIC) {
        Some(AiProvenance::AiEdited)
    } else {
        None
    }
}

/// Calcule la provenance d'un asset à l'ingestion.
///
/// Priorité : une **déclaration explicite** (`declared`, ex. champ d'upload) prime sur la
/// détection par marqueurs, car elle traduit une affirmation volontaire de l'éditeur — qui
/// engage sa responsabilité au titre de l'art. 50. À défaut de déclaration exploitable, on
/// retient les marqueurs embarqués, sinon `Unknown` (aucune affirmation non fondée).
///
/// `c2pa_present` est toujours dérivé des octets (fait technique vérifiable).
pub fn derive(bytes: &[u8], declared: Option<&str>, generator: Option<&str>) -> Provenance {
    let declared_ai = declared
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(AiProvenance::from_token);

    let ai = match declared_ai {
        // Déclaration exploitable (non « unknown ») → elle fait foi.
        Some(p) if p != AiProvenance::Unknown => p,
        // Sinon, on s'appuie sur les marqueurs embarqués, à défaut Unknown.
        _ => ai_from_markers(bytes).unwrap_or(AiProvenance::Unknown),
    };

    Provenance {
        ai,
        c2pa_present: detect_c2pa(bytes),
        generator: generator
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
    }
}

/// Recherche de sous-séquence d'octets (naïve, suffisante pour des marqueurs courts).
fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() || haystack.len() < needle.len() {
        return needle.is_empty();
    }
    haystack.windows(needle.len()).any(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fabrique un faux conteneur JUMBF/C2PA minimal pour les tests.
    fn jumbf_c2pa(extra: &[u8]) -> Vec<u8> {
        let mut v = b"\x00\x00\x00\x20jumb\x00\x00\x00\x18jumdc2pa".to_vec();
        v.extend_from_slice(extra);
        v
    }

    #[test]
    fn c2pa_requires_both_markers() {
        assert!(detect_c2pa(&jumbf_c2pa(b"")), "jumb + c2pa => présent");
        assert!(
            !detect_c2pa(b"juste du texte c2pa"),
            "c2pa seul ne suffit pas"
        );
        assert!(!detect_c2pa(b"jumb sans label"), "jumb seul ne suffit pas");
        assert!(!detect_c2pa(b""), "vide => absent");
    }

    #[test]
    fn markers_detect_generated() {
        let bytes = jumbf_c2pa(br#"{"digitalSourceType":"...trainedAlgorithmicMedia"}"#);
        assert_eq!(ai_from_markers(&bytes), Some(AiProvenance::AiGenerated));
    }

    #[test]
    fn markers_detect_composite_as_edited() {
        let bytes = br#"{"digitalSourceType":"http://cv.iptc.org/.../compositeWithTrainedAlgorithmicMedia"}"#;
        // Le composite doit l'emporter sur le sous-marqueur « trainedAlgorithmicMedia ».
        assert_eq!(ai_from_markers(bytes), Some(AiProvenance::AiEdited));
    }

    #[test]
    fn no_marker_no_ai() {
        assert_eq!(ai_from_markers(b"photo brute sans metadata"), None);
    }

    #[test]
    fn declared_hint_overrides_markers() {
        // Octets sans marqueur, mais l'éditeur déclare « ai-generated ».
        let p = derive(b"raw", Some("ai-generated"), None);
        assert_eq!(p.ai, AiProvenance::AiGenerated);
    }

    #[test]
    fn declaration_unknown_falls_back_to_markers() {
        let bytes = jumbf_c2pa(br#""trainedAlgorithmicMedia""#);
        // Déclaration vide/inconnue → on retient les marqueurs embarqués (généré) + C2PA.
        let p = derive(&bytes, Some(""), None);
        assert_eq!(p.ai, AiProvenance::AiGenerated);
        assert!(p.c2pa_present);
    }

    #[test]
    fn human_when_nothing_found() {
        let p = derive(b"photo argentique", None, None);
        assert_eq!(p.ai, AiProvenance::Unknown);
        assert!(!p.c2pa_present);
        assert_eq!(p.generator, None);
    }

    #[test]
    fn generator_is_trimmed_and_optional() {
        assert_eq!(derive(b"x", None, Some("  ")).generator, None);
        assert_eq!(
            derive(b"x", Some("ai"), Some(" Firefly ")).generator,
            Some("Firefly".to_string())
        );
    }
}
