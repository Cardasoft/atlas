//! atlas-render — renditions & crop intelligent (doc 06/34).
//!
//! TDD : la **géométrie** (redimensionnement préservant le ratio, cadre de crop par ratio
//! centré sur le point focal) est pure et testée. Le décodage/encodage réel (libvips/FFmpeg)
//! est isolé derrière le trait `ImageProcessor` ; un `NoopProcessor` sert les tests.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Size {
    pub w: u32,
    pub h: u32,
}

/// Redimensionne `src` pour tenir dans un carré `max` en préservant le ratio (fit).
/// Ne fait jamais d'agrandissement (≤ taille source).
pub fn fit_within(src: Size, max: u32) -> Size {
    if src.w == 0 || src.h == 0 {
        return Size { w: 0, h: 0 };
    }
    let longest = src.w.max(src.h);
    if longest <= max {
        return src;
    }
    let scale = max as f64 / longest as f64;
    Size {
        w: ((src.w as f64 * scale).round() as u32).max(1),
        h: ((src.h as f64 * scale).round() as u32).max(1),
    }
}

/// Cadre de crop maximal du ratio `ratio_w:ratio_h`, centré sur le point focal `(fx, fy)`,
/// clampé aux bornes de l'image source (doc 34 §4.2).
pub fn crop_box_for_ratio(src: Size, focal: (u32, u32), ratio_w: u32, ratio_h: u32) -> Rect {
    if src.w == 0 || src.h == 0 || ratio_w == 0 || ratio_h == 0 {
        return Rect {
            x: 0,
            y: 0,
            w: src.w,
            h: src.h,
        };
    }
    // Plus grande fenêtre du ratio tenant dans la source.
    let target = ratio_w as f64 / ratio_h as f64;
    let srcr = src.w as f64 / src.h as f64;
    let (mut w, mut h) = if srcr > target {
        // source plus large → hauteur limitante
        let h = src.h as f64;
        (h * target, h)
    } else {
        let w = src.w as f64;
        (w, w / target)
    };
    w = w.min(src.w as f64);
    h = h.min(src.h as f64);
    let (w, h) = (w.round() as u32, h.round() as u32);

    // Centrer sur le focal, puis clamp dans [0, src - dim].
    let half_w = w / 2;
    let half_h = h / 2;
    let x = clamp_start(focal.0, half_w, src.w, w);
    let y = clamp_start(focal.1, half_h, src.h, h);
    Rect { x, y, w, h }
}

fn clamp_start(focal: u32, half: u32, src_len: u32, win: u32) -> u32 {
    let max_start = src_len.saturating_sub(win);
    focal.saturating_sub(half).min(max_start)
}

/// Décodage/encodage image réel (libvips). Isolé pour rester testable.
pub trait ImageProcessor: Send + Sync {
    /// Produit une vignette ≤ `max` (octets encodés). M1 : implémentation réelle = libvips.
    fn thumbnail(&self, input: &[u8], max: u32) -> Result<Vec<u8>, RenderError>;
}

#[derive(Debug, thiserror::Error)]
pub enum RenderError {
    #[error("traitement image: {0}")]
    Process(String),
}

/// Processeur sans-op (tests/dev) : renvoie l'entrée inchangée.
pub struct NoopProcessor;
impl ImageProcessor for NoopProcessor {
    fn thumbnail(&self, input: &[u8], _max: u32) -> Result<Vec<u8>, RenderError> {
        Ok(input.to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_preserves_ratio_and_caps_longest_side() {
        let s = fit_within(Size { w: 4000, h: 2000 }, 1000);
        assert_eq!(s, Size { w: 1000, h: 500 });
    }

    #[test]
    fn fit_never_upscales() {
        let s = fit_within(Size { w: 800, h: 600 }, 2000);
        assert_eq!(s, Size { w: 800, h: 600 });
    }

    #[test]
    fn square_crop_from_landscape_is_centered() {
        // 1000x500, focal au centre, ratio 1:1 → carré 500x500 centré (x=250).
        let r = crop_box_for_ratio(Size { w: 1000, h: 500 }, (500, 250), 1, 1);
        assert_eq!(
            r,
            Rect {
                x: 250,
                y: 0,
                w: 500,
                h: 500
            }
        );
    }

    #[test]
    fn crop_clamps_at_edges() {
        // focal à l'extrême gauche → la fenêtre est collée au bord (x=0).
        let r = crop_box_for_ratio(Size { w: 1000, h: 500 }, (0, 250), 1, 1);
        assert_eq!(r.x, 0);
        assert_eq!(r.w, 500);
    }

    #[test]
    fn crop_respects_target_ratio() {
        // 16:9 dans une source 1000x1000 → largeur limitante 1000, hauteur 1000*9/16=562,5→563.
        let r = crop_box_for_ratio(Size { w: 1000, h: 1000 }, (500, 500), 16, 9);
        assert_eq!(r.w, 1000);
        assert_eq!(r.h, 563);
    }

    #[test]
    fn noop_processor_passes_through() {
        let p = NoopProcessor;
        assert_eq!(p.thumbnail(b"img", 256).unwrap(), b"img");
    }
}
