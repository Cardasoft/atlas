//! atlas-embed — encodage d'embeddings de requête (doc 25 §4.2).
//!
//! Démarche TDD : les tests en bas décrivent le **contrat** attendu d'un `Embedder`
//! (dimension fixe, déterminisme, normalisation L2). Le `FakeEmbedder` les satisfait
//! sans modèle ni dépendance externe ; il sera remplacé par SigLIP (ort/Candle) en
//! conservant ce contrat et donc ces tests.

/// Pré/post-traitement déterministe (sans modèle ni `ort`) partagé par le `FakeEmbedder`
/// et le futur `SiglipEmbedder` : canonicalisation texte, normalisation L2, cosinus.
pub mod preprocess;

/// Dimension de l'espace multimodal partagé (SigLIP so400m). Doit coïncider avec
/// la colonne `embedding.vec vector(1152)` de la migration 0001.
pub const EMBED_DIM: usize = 1152;

/// Contrat d'un encodeur de texte → vecteur (in-process, doc 25).
pub trait Embedder: Send + Sync {
    /// Dimension des vecteurs produits.
    fn dim(&self) -> usize;
    /// Encode un texte en vecteur normalisé (L2 = 1) dans l'espace multimodal.
    fn encode(&self, text: &str) -> Vec<f32>;
}

/// Encodeur factice **déterministe** pour le développement et les tests.
/// Produit un vecteur reproductible à partir du texte, normalisé L2.
#[derive(Debug, Clone, Default)]
pub struct FakeEmbedder;

impl FakeEmbedder {
    /// FNV-1a 64 bits — hachage déterministe et stable (pas de dépendance externe).
    fn fnv1a(bytes: &[u8]) -> u64 {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for &b in bytes {
            h ^= b as u64;
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        h
    }
}

impl Embedder for FakeEmbedder {
    fn dim(&self) -> usize {
        EMBED_DIM
    }

    fn encode(&self, text: &str) -> Vec<f32> {
        // Canonicalisation déterministe partagée avec le futur encodeur réel (SigLIP).
        let text = preprocess::prepare_text(text);
        // Générateur congruentiel linéaire amorcé par le hash du texte → vecteur stable.
        let mut state = Self::fnv1a(text.as_bytes()).max(1);
        let mut v = Vec::with_capacity(EMBED_DIM);
        for _ in 0..EMBED_DIM {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            // map vers [-1, 1]
            let x = ((state >> 33) as f32 / (1u64 << 31) as f32) - 1.0;
            v.push(x);
        }
        // Normalisation L2 (sphère unité) — fonction pure partagée (preprocess).
        preprocess::normalize_l2(&mut v);
        v
    }
}

/// Seam de l'encodeur réel SigLIP (ONNX Runtime). Activée par la feature `ml`.
/// M1 : structure + contrat ; le chargement du modèle (poids mirrorés) et l'inférence
/// `ort` s'implémentent ici sans changer le trait `Embedder` ni ses appelants.
#[cfg(feature = "ml")]
pub struct SiglipEmbedder {
    // session: ort::Session,  // chargée depuis un modèle local mirroré
}

#[cfg(feature = "ml")]
impl SiglipEmbedder {
    /// Charge le modèle depuis un chemin local (jamais de téléchargement runtime).
    pub fn from_path(_model_path: &str) -> Result<Self, String> {
        Err("SiglipEmbedder: implémentation ort à fournir (M1+)".into())
    }
}

#[cfg(feature = "ml")]
impl Embedder for SiglipEmbedder {
    fn dim(&self) -> usize {
        EMBED_DIM
    }
    fn encode(&self, _text: &str) -> Vec<f32> {
        // Pipeline cible : preprocess::prepare_text → tokenisation → inférence ort →
        // preprocess::normalize_l2. À brancher avec le modèle local + ONNX Runtime.
        unimplemented!("inférence SigLIP via ort — à brancher (M1+, env outillé)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn l2(v: &[f32]) -> f32 {
        v.iter().map(|x| x * x).sum::<f32>().sqrt()
    }

    #[test]
    fn dim_matches_schema() {
        let e = FakeEmbedder;
        assert_eq!(e.dim(), EMBED_DIM);
        assert_eq!(e.encode("plage").len(), EMBED_DIM);
    }

    #[test]
    fn encoding_is_deterministic() {
        let e = FakeEmbedder;
        assert_eq!(e.encode("coucher de soleil"), e.encode("coucher de soleil"));
    }

    #[test]
    fn different_text_different_vector() {
        let e = FakeEmbedder;
        assert_ne!(e.encode("plage"), e.encode("montagne"));
    }

    #[test]
    fn vectors_are_l2_normalized() {
        let e = FakeEmbedder;
        let n = l2(&e.encode("ambiance estivale"));
        assert!((n - 1.0).abs() < 1e-4, "norme L2 attendue ~1, obtenue {n}");
    }
}
