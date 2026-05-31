//! Fusion Reciprocal Rank Fusion (doc 25 §4.4).
//! Combine plusieurs listes classées + signaux, avec tie-break stable (pagination).

use uuid::Uuid;

/// Constante RRF (doc 25). Amortit l'impact des rangs élevés.
pub const K_RRF: f32 = 60.0;

/// Pondérations configurables par tenant (doc 25 §4.4 / §9).
#[derive(Debug, Clone)]
pub struct Weights {
    pub semantic: f32,
    pub lexical: f32,
}

impl Default for Weights {
    fn default() -> Self {
        Self {
            semantic: 1.0,
            lexical: 1.0,
        }
    }
}

/// Un résultat candidat issu d'une voie de récupération (vectorielle ou lexicale).
/// `rank` = position 0-based dans sa liste d'origine.
#[derive(Debug, Clone, Copy)]
pub struct Ranked {
    pub asset_id: Uuid,
    pub rank: usize,
}

/// Résultat fusionné, trié par score décroissant puis id (tie-break stable).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Scored {
    pub asset_id: Uuid,
    pub score: f32,
}

/// Fusionne les listes vectorielle et lexicale (RRF pondéré).
pub fn fuse(vector_hits: &[Ranked], lexical_hits: &[Ranked], w: &Weights) -> Vec<Scored> {
    use std::collections::HashMap;
    let mut acc: HashMap<Uuid, f32> = HashMap::new();

    for h in vector_hits {
        *acc.entry(h.asset_id).or_insert(0.0) += w.semantic / (K_RRF + h.rank as f32 + 1.0);
    }
    for h in lexical_hits {
        *acc.entry(h.asset_id).or_insert(0.0) += w.lexical / (K_RRF + h.rank as f32 + 1.0);
    }

    let mut out: Vec<Scored> = acc
        .into_iter()
        .map(|(asset_id, score)| Scored { asset_id, score })
        .collect();

    // Tri stable : score décroissant, puis id croissant (déterminisme pour la pagination).
    out.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.asset_id.cmp(&b.asset_id))
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    #[test]
    fn fusion_prefers_items_ranked_high_in_both() {
        // L'asset 1 est bien classé dans les deux voies → doit dominer.
        let vec_hits = vec![
            Ranked { asset_id: id(1), rank: 0 },
            Ranked { asset_id: id(2), rank: 1 },
        ];
        let lex_hits = vec![
            Ranked { asset_id: id(1), rank: 0 },
            Ranked { asset_id: id(3), rank: 1 },
        ];
        let out = fuse(&vec_hits, &lex_hits, &Weights::default());
        assert_eq!(out[0].asset_id, id(1));
        assert!(out[0].score > out[1].score);
    }

    #[test]
    fn fusion_is_stable_for_ties() {
        // Deux assets à égalité de score → ordre déterministe par id (pagination stable).
        let vec_hits = vec![
            Ranked { asset_id: id(10), rank: 0 },
            Ranked { asset_id: id(5), rank: 0 },
        ];
        let out = fuse(&vec_hits, &[], &Weights::default());
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].asset_id, id(5)); // id plus petit en premier à score égal
        assert_eq!(out[1].asset_id, id(10));
    }

    #[test]
    fn weights_shift_balance() {
        let vec_hits = vec![Ranked { asset_id: id(1), rank: 0 }];
        let lex_hits = vec![Ranked { asset_id: id(2), rank: 0 }];
        let w = Weights { semantic: 3.0, lexical: 1.0 };
        let out = fuse(&vec_hits, &lex_hits, &w);
        assert_eq!(out[0].asset_id, id(1)); // le poids sémantique fait gagner l'asset vectoriel
    }
}
