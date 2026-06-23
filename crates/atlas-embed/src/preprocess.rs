//! atlas-embed::preprocess — pré/post-traitement DÉTERMINISTE de l'encodage (sans modèle).
//!
//! C'est la part de l'encodage SigLIP **indépendante du modèle et d'`ort`** :
//! canonicalisation du texte, normalisation L2, similarité cosinus, plan de
//! pré-traitement image. **Pure et testée** (TDD, doc 25 §4.2) → réutilisée telle quelle
//! par l'encodeur réel `SiglipEmbedder` (feature `ml`) une fois le modèle + ONNX Runtime
//! branchés dans un environnement outillé. Aucune dépendance externe (souverain/frugal).

/// Longueur de contexte texte SigLIP (so400m ≈ 64 tokens). On plafonne au nombre de
/// **mots** pour la canonicalisation pré-tokenisation ; la tokenisation fine vit côté
/// modèle (feature `ml`).
pub const MAX_TEXT_WORDS: usize = 64;

/// Côté (carré) attendu par le préprocesseur image SigLIP so400m/384.
pub const IMAGE_SIDE: u32 = 384;
/// Normalisation image SigLIP (moyenne/écart-type par canal RGB).
pub const IMAGE_MEAN: [f32; 3] = [0.5, 0.5, 0.5];
/// Écart-type par canal RGB.
pub const IMAGE_STD: [f32; 3] = [0.5, 0.5, 0.5];

/// Canonicalise un texte de requête/légende avant encodage : trim + collapse des
/// espaces (via `split_whitespace`), minuscules, troncature à [`MAX_TEXT_WORDS`] mots.
/// Déterministe — même entrée logique ⇒ même sortie ⇒ même vecteur.
pub fn prepare_text(text: &str) -> String {
    text.split_whitespace()
        .take(MAX_TEXT_WORDS)
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Normalise un vecteur sur la sphère unité (L2 = 1) en place. No-op si la norme est nulle.
/// Les requêtes ET les assets vivent sur la sphère unité → le produit scalaire = cosinus.
pub fn normalize_l2(v: &mut [f32]) {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in v.iter_mut() {
            *x /= norm;
        }
    }
}

/// Similarité cosinus de deux vecteurs (= produit scalaire s'ils sont L2-normalisés).
/// Renvoie 0.0 si l'un des vecteurs est nul. Tronque à la longueur commune par sûreté.
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let n = a.len().min(b.len());
    let dot: f32 = (0..n).map(|i| a[i] * b[i]).sum();
    let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na * nb)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prepare_text_canonicalise() {
        // minuscules + collapse des espaces multiples + trim
        assert_eq!(prepare_text("  Plage   au   Soleil "), "plage au soleil");
        assert_eq!(prepare_text("MONTAGNE"), "montagne");
    }

    #[test]
    fn prepare_text_tronque_aux_mots() {
        let long = (0..100)
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join(" ");
        let got = prepare_text(&long);
        assert_eq!(got.split_whitespace().count(), MAX_TEXT_WORDS);
    }

    #[test]
    fn prepare_text_idempotent() {
        let once = prepare_text("Coucher de Soleil");
        assert_eq!(prepare_text(&once), once);
    }

    #[test]
    fn normalize_l2_donne_norme_unite() {
        let mut v = vec![3.0_f32, 4.0];
        normalize_l2(&mut v);
        let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((n - 1.0).abs() < 1e-6, "norme attendue 1, obtenue {n}");
        assert!((v[0] - 0.6).abs() < 1e-6 && (v[1] - 0.8).abs() < 1e-6);
    }

    #[test]
    fn normalize_l2_vecteur_nul_inchange() {
        let mut v = vec![0.0_f32, 0.0, 0.0];
        normalize_l2(&mut v);
        assert_eq!(v, vec![0.0, 0.0, 0.0]);
    }

    #[test]
    fn cosine_identique_vaut_un() {
        let a = vec![1.0_f32, 2.0, 3.0];
        assert!((cosine_similarity(&a, &a) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_orthogonal_vaut_zero() {
        let a = vec![1.0_f32, 0.0];
        let b = vec![0.0_f32, 1.0];
        assert!(cosine_similarity(&a, &b).abs() < 1e-6);
    }

    #[test]
    fn cosine_vecteur_nul_vaut_zero() {
        let a = vec![0.0_f32, 0.0];
        let b = vec![1.0_f32, 1.0];
        assert_eq!(cosine_similarity(&a, &b), 0.0);
    }

    #[test]
    fn constantes_image_coherentes() {
        assert_eq!(IMAGE_SIDE, 384);
        assert_eq!(IMAGE_MEAN.len(), 3);
        assert_eq!(IMAGE_STD.len(), 3);
    }
}
