//! Métriques de qualité de classement, calculées **hors-ligne** (doc 25 §6/§10.3).
//!
//! Servent le « golden set » `requête → assets pertinents` : on compare l'ordre rendu par la
//! recherche à un jugement de pertinence pour mesurer **nDCG@10 ≥ 0,85** et **précision@5**
//! (gate CI bloquant, §10.8). Fonctions **pures** (aucune base, aucun réseau) : exécutables
//! en air-gap et testables sur valeurs connues. Les jugements peuvent provenir d'annotations
//! curées ou des clics agrégés du `search_log` (gains gradués).

use std::collections::{HashMap, HashSet};
use uuid::Uuid;

/// Gain cumulé actualisé (DCG@k) de l'ordre `ranked`, selon les `gains` par asset.
/// Réduction logarithmique standard : position `i` (0-based) pondérée par `1/log2(i+2)`.
/// Un asset absent de `gains` compte pour un gain nul.
pub fn dcg_at_k(ranked: &[Uuid], gains: &HashMap<Uuid, f32>, k: usize) -> f64 {
    ranked
        .iter()
        .take(k)
        .enumerate()
        .map(|(i, id)| {
            let gain = gains.get(id).copied().unwrap_or(0.0) as f64;
            gain / ((i + 2) as f64).log2()
        })
        .sum()
}

/// nDCG@k : DCG@k normalisé par le DCG@k de l'ordre **idéal** (gains décroissants).
/// Renvoie `0.0` si aucun gain positif n'existe (IDCG nul → évite la division par zéro).
/// Borné dans `[0, 1]`.
pub fn ndcg_at_k(ranked: &[Uuid], gains: &HashMap<Uuid, f32>, k: usize) -> f64 {
    let dcg = dcg_at_k(ranked, gains, k);
    // Ordre idéal : les meilleurs gains d'abord, indépendamment des ids.
    let mut ideal: Vec<f32> = gains.values().copied().filter(|g| *g > 0.0).collect();
    ideal.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
    let idcg: f64 = ideal
        .iter()
        .take(k)
        .enumerate()
        .map(|(i, &g)| g as f64 / ((i + 2) as f64).log2())
        .sum();
    if idcg == 0.0 {
        0.0
    } else {
        dcg / idcg
    }
}

/// Précision@k : fraction des `k` premiers résultats jugés pertinents. Dénominateur = `k`
/// (un résultat manquant compte comme non pertinent), conformément à la métrique standard.
/// `k = 0` → `0.0`.
pub fn precision_at_k(ranked: &[Uuid], relevant: &HashSet<Uuid>, k: usize) -> f64 {
    if k == 0 {
        return 0.0;
    }
    let hits = ranked.iter().take(k).filter(|id| relevant.contains(id)).count();
    hits as f64 / k as f64
}

/// nDCG@k moyen sur un ensemble de cas `(ordre_rendu, gains)` (golden set). Ensemble vide → `0.0`.
pub fn mean_ndcg_at_k(cases: &[(Vec<Uuid>, HashMap<Uuid, f32>)], k: usize) -> f64 {
    if cases.is_empty() {
        return 0.0;
    }
    let sum: f64 = cases.iter().map(|(ranked, gains)| ndcg_at_k(ranked, gains, k)).sum();
    sum / cases.len() as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    const EPS: f64 = 1e-6;

    #[test]
    fn perfect_ranking_scores_one() {
        let (a, b, c) = (id(1), id(2), id(3));
        let gains = HashMap::from([(a, 1.0), (b, 0.0), (c, 1.0)]);
        // Ordre idéal : les deux pertinents d'abord.
        let ranked = vec![a, c, b];
        assert!((ndcg_at_k(&ranked, &gains, 3) - 1.0).abs() < EPS);
    }

    #[test]
    fn imperfect_ranking_known_value() {
        let (a, b, c) = (id(1), id(2), id(3));
        let gains = HashMap::from([(a, 1.0), (b, 0.0), (c, 1.0)]);
        // ranked [a,b,c] : DCG = 1/log2(2) + 0 + 1/log2(4) = 1.0 + 0.5 = 1.5
        // IDCG (ordre [a,c]) = 1/log2(2) + 1/log2(3) = 1 + 0.6309298 = 1.6309298
        // nDCG = 1.5 / 1.6309298 = 0.9197208
        let got = ndcg_at_k(&[a, b, c], &gains, 3);
        assert!((got - 0.9197208).abs() < 1e-6, "nDCG attendu ≈ 0.9197, obtenu {got}");
    }

    #[test]
    fn dcg_uses_log_discount() {
        let (a, b) = (id(1), id(2));
        let gains = HashMap::from([(a, 3.0), (b, 2.0)]);
        // 3/log2(2) + 2/log2(3) = 3 + 1.2618595 = 4.2618595
        let got = dcg_at_k(&[a, b], &gains, 2);
        assert!((got - 4.2618595).abs() < 1e-6, "DCG obtenu {got}");
    }

    #[test]
    fn ndcg_zero_when_no_relevant() {
        let gains = HashMap::from([(id(1), 0.0), (id(2), 0.0)]);
        assert_eq!(ndcg_at_k(&[id(1), id(2)], &gains, 2), 0.0);
    }

    #[test]
    fn ndcg_respects_k_cutoff() {
        let (a, b, c) = (id(1), id(2), id(3));
        // Le seul pertinent est en 3e position → invisible à k=2 → nDCG@2 = 0.
        let gains = HashMap::from([(a, 0.0), (b, 0.0), (c, 1.0)]);
        assert_eq!(ndcg_at_k(&[a, b, c], &gains, 2), 0.0);
        // À k=3 il devient visible → nDCG@3 > 0.
        assert!(ndcg_at_k(&[a, b, c], &gains, 3) > 0.0);
    }

    #[test]
    fn precision_counts_hits_over_k() {
        let (a, b, c) = (id(1), id(2), id(3));
        let relevant = HashSet::from([a, c]);
        assert!((precision_at_k(&[a, b, c], &relevant, 3) - 2.0 / 3.0).abs() < EPS);
        assert!((precision_at_k(&[a, b, c], &relevant, 2) - 0.5).abs() < EPS);
        assert_eq!(precision_at_k(&[a, b, c], &relevant, 0), 0.0);
    }

    #[test]
    fn mean_ndcg_averages_cases() {
        let (a, b, c) = (id(1), id(2), id(3));
        let gains = HashMap::from([(a, 1.0), (b, 0.0), (c, 1.0)]);
        let cases = vec![
            (vec![a, c, b], gains.clone()), // parfait → 1.0
            (vec![b, a, c], gains.clone()), // imparfait → < 1.0
        ];
        let mean = mean_ndcg_at_k(&cases, 3);
        assert!(mean > 0.0 && mean < 1.0, "moyenne entre les deux cas, obtenu {mean}");
        assert_eq!(mean_ndcg_at_k(&[], 10), 0.0);
    }
}
