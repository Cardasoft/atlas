//! Empreintes d'asset (doc 26 §5-6) — fonctions pures, testées en premier (TDD).
//! - `sha256_hex` : déduplication **exacte** (déterministe, vecteurs connus).
//! - `average_hash` + `hamming` : déduplication **perceptuelle** (quasi-doublons).

use sha2::{Digest, Sha256};

/// SHA-256 d'un contenu, encodé en hexadécimal minuscule.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    let digest = h.finalize();
    let mut out = String::with_capacity(64);
    for b in digest {
        out.push(nibble(b >> 4));
        out.push(nibble(b & 0x0f));
    }
    out
}

fn nibble(n: u8) -> char {
    match n {
        0..=9 => (b'0' + n) as char,
        _ => (b'a' + (n - 10)) as char,
    }
}

/// Average-hash (aHash) sur une grille de luminance 8×8 (64 octets, 0..=255).
/// Bit = 1 si le pixel est ≥ moyenne. Robuste au redimensionnement/compression légère.
/// Renvoie 0 si l'entrée n'a pas exactement 64 valeurs (entrée invalide).
pub fn average_hash(luma_8x8: &[u8]) -> u64 {
    if luma_8x8.len() != 64 {
        return 0;
    }
    let sum: u32 = luma_8x8.iter().map(|&p| p as u32).sum();
    let mean = (sum / 64) as u8;
    let mut bits: u64 = 0;
    for (i, &p) in luma_8x8.iter().enumerate() {
        if p >= mean {
            bits |= 1u64 << i;
        }
    }
    bits
}

/// Distance de Hamming entre deux hashes perceptuels (nb de bits différents).
pub fn hamming(a: u64, b: u64) -> u32 {
    (a ^ b).count_ones()
}

/// Deux assets sont des quasi-doublons si leur distance perceptuelle ≤ seuil.
pub fn is_near_duplicate(a: u64, b: u64, threshold: u32) -> bool {
    hamming(a, b) <= threshold
}

#[cfg(test)]
mod tests {
    use super::*;

    // Vecteurs SHA-256 standard (FIPS 180-4) — le contrat est figé.
    #[test]
    fn sha256_empty_known_vector() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn sha256_abc_known_vector() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn sha256_is_deterministic() {
        assert_eq!(sha256_hex(b"atlas"), sha256_hex(b"atlas"));
        assert_ne!(sha256_hex(b"atlas"), sha256_hex(b"atlap"));
    }

    #[test]
    fn average_hash_rejects_bad_input() {
        assert_eq!(average_hash(&[0u8; 10]), 0);
    }

    #[test]
    fn identical_images_have_zero_distance() {
        let img = [128u8; 64];
        assert_eq!(hamming(average_hash(&img), average_hash(&img)), 0);
        assert!(is_near_duplicate(average_hash(&img), average_hash(&img), 0));
    }

    #[test]
    fn different_images_have_positive_distance() {
        let uniform = [128u8; 64];
        let mut half = [0u8; 64];
        for p in half.iter_mut().take(32) {
            *p = 255;
        }
        let d = hamming(average_hash(&uniform), average_hash(&half));
        assert!(
            d > 0,
            "des images distinctes doivent différer (distance {d})"
        );
    }

    #[test]
    fn hamming_counts_differing_bits() {
        assert_eq!(hamming(0b1010, 0b0011), 2);
    }
}
