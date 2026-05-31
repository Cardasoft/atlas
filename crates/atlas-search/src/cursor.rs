//! Pagination par curseur stable (doc 25 §4.6).
//!
//! Jeton **opaque** encodant la position de tri `(score, asset_id)` du dernier résultat
//! servi. La page suivante reprend l'ordre fusionné déterministe (score décroissant,
//! id croissant) sans `OFFSET` : aucun doublon ni saut entre pages, même si l'index
//! évolue (re-fusion déterministe). Sans dépendance externe (souverain).

use crate::rrf::Scored;
use uuid::Uuid;

/// Position de tri stable d'un résultat (doc 25 §4.6).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Cursor {
    pub score: f32,
    pub asset_id: Uuid,
    /// Hash de la requête : lie le curseur à SA recherche. Un curseur présenté avec une
    /// autre requête est rejeté par le handler (→ page 1), évitant un fenêtrage incohérent.
    pub query_hash: u64,
}

impl Cursor {
    /// Encode en jeton opaque url-safe. Le score est sérialisé par ses **bits exacts**
    /// (aucune perte de précision) pour que la comparaison de page reste fidèle ; le
    /// `query_hash` est joint pour lier le curseur à sa requête.
    pub fn encode(&self) -> String {
        let raw = format!(
            "{:08x}:{}:{:016x}",
            self.score.to_bits(),
            self.asset_id.simple(),
            self.query_hash
        );
        to_hex(raw.as_bytes())
    }

    /// Décode un jeton ; `None` si malformé (le handler ignore alors le curseur → page 1).
    pub fn decode(token: &str) -> Option<Self> {
        let raw = from_hex(token)?;
        let s = std::str::from_utf8(&raw).ok()?;
        let mut parts = s.split(':');
        let bits = parts.next()?;
        let id = parts.next()?;
        let qh = parts.next()?;
        if parts.next().is_some() {
            return None; // champs surnuméraires → jeton invalide
        }
        Some(Self {
            score: f32::from_bits(u32::from_str_radix(bits, 16).ok()?),
            asset_id: Uuid::parse_str(id).ok()?,
            query_hash: u64::from_str_radix(qh, 16).ok()?,
        })
    }
}

/// `true` si `item` se situe **strictement après** le curseur dans l'ordre fusionné
/// (score décroissant, puis id croissant) — donc appartient à une page ultérieure.
fn is_after(item: &Scored, c: &Cursor) -> bool {
    item.score < c.score || (item.score == c.score && item.asset_id > c.asset_id)
}

/// Découpe une page dans la liste fusionnée triée. Renvoie la page et le curseur suivant
/// (`None` s'il n'y a plus de résultats au-delà). `query_hash` est estampillé sur le curseur
/// suivant pour le lier à la requête courante (doc 25 §4.6).
pub fn paginate(
    sorted: &[Scored],
    cursor: Option<Cursor>,
    page_size: usize,
    query_hash: u64,
) -> (Vec<Scored>, Option<Cursor>) {
    let start = match cursor {
        Some(c) => sorted.iter().position(|it| is_after(it, &c)).unwrap_or(sorted.len()),
        None => 0,
    };
    let page: Vec<Scored> = sorted[start..].iter().take(page_size).copied().collect();
    let next = match page.last() {
        Some(last) if start + page.len() < sorted.len() => Some(Cursor {
            score: last.score,
            asset_id: last.asset_id,
            query_hash,
        }),
        _ => None,
    };
    (page, next)
}

fn to_hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(char::from_digit((b >> 4) as u32, 16).unwrap());
        s.push(char::from_digit((b & 0xf) as u32, 16).unwrap());
    }
    s
}

fn from_hex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    let b = s.as_bytes();
    (0..b.len())
        .step_by(2)
        .map(|i| {
            let hi = (b[i] as char).to_digit(16)?;
            let lo = (b[i + 1] as char).to_digit(16)?;
            Some(((hi << 4) | lo) as u8)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    #[test]
    fn cursor_round_trip() {
        let c = Cursor { score: 0.123_456_79, asset_id: id(42), query_hash: 0xdead_beef_cafe_0001 };
        let back = Cursor::decode(&c.encode()).expect("decode");
        assert_eq!(c, back);
        // Bits exacts → pas de dérive de score.
        assert_eq!(c.score.to_bits(), back.score.to_bits());
        assert_eq!(c.query_hash, back.query_hash);
    }

    #[test]
    fn decode_rejects_garbage() {
        assert!(Cursor::decode("not-hex!").is_none());
        assert!(Cursor::decode("zz").is_none());
        assert!(Cursor::decode(&to_hex(b"no-colon-here")).is_none());
        // Ancien format à 2 champs (sans query_hash) → rejeté.
        assert!(Cursor::decode(&to_hex(b"3f800000:00000000000000000000000000000001")).is_none());
    }

    #[test]
    fn pagination_covers_all_without_overlap_or_gap() {
        // 7 résultats triés (scores distincts) ; pages de 3 → 3 + 3 + 1.
        let sorted: Vec<Scored> = (0..7)
            .map(|i| Scored { asset_id: id(i as u128), score: 1.0 - i as f32 * 0.1 })
            .collect();

        let mut seen = Vec::new();
        let mut cur: Option<Cursor> = None;
        loop {
            let (page, next) = paginate(&sorted, cur, 3, 0);
            seen.extend(page.iter().map(|s| s.asset_id));
            match next {
                Some(c) => cur = Some(c),
                None => break,
            }
        }
        let expected: Vec<Uuid> = sorted.iter().map(|s| s.asset_id).collect();
        assert_eq!(seen, expected, "couverture complète, ordre conservé, aucun doublon/saut");
    }

    #[test]
    fn pagination_stable_across_tied_scores() {
        // Scores égaux → tie-break par id ; le curseur doit reprendre exactement après.
        let sorted: Vec<Scored> = (0..5)
            .map(|i| Scored { asset_id: id(i as u128), score: 0.5 })
            .collect();
        let (p1, next) = paginate(&sorted, None, 2, 0);
        assert_eq!(p1.iter().map(|s| s.asset_id).collect::<Vec<_>>(), vec![id(0), id(1)]);
        let (p2, _) = paginate(&sorted, next, 2, 0);
        assert_eq!(p2.iter().map(|s| s.asset_id).collect::<Vec<_>>(), vec![id(2), id(3)]);
    }

    #[test]
    fn last_page_has_no_next_cursor() {
        let sorted: Vec<Scored> = (0..2)
            .map(|i| Scored { asset_id: id(i as u128), score: 1.0 - i as f32 })
            .collect();
        let (_page, next) = paginate(&sorted, None, 10, 0);
        assert!(next.is_none());
    }
}
