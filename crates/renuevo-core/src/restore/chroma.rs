//! Ruido de color con un filtro guiado (He et al., 2010) autoguiado: el propio plano de color
//! decide qué conservar.
//!
//! Las variaciones de color del orden del ruido medido se promedian; los bordes de color mayores
//! se conservan aunque apenas cambie la luminancia (flores rojas sobre hojas verdes, franjas de
//! color). Guiarlo por la luminancia los emborronaba. Coste O(1) por píxel gracias a `box_blur`.

use rayon::prelude::*;

use crate::filters::box_blur;

pub fn guided_filter(
    guide: &[f32],
    src: &[f32],
    w: usize,
    h: usize,
    r: usize,
    eps: f32,
) -> Vec<f32> {
    let mean_i = box_blur(guide, w, h, r);
    let mean_p = box_blur(src, w, h, r);
    let ip: Vec<f32> = guide.par_iter().zip(src).map(|(i, p)| i * p).collect();
    let ii: Vec<f32> = guide.par_iter().map(|i| i * i).collect();
    let corr_ip = box_blur(&ip, w, h, r);
    let corr_ii = box_blur(&ii, w, h, r);

    let (a, b): (Vec<f32>, Vec<f32>) = (0..w * h)
        .into_par_iter()
        .map(|k| {
            let var_i = corr_ii[k] - mean_i[k] * mean_i[k];
            let cov_ip = corr_ip[k] - mean_i[k] * mean_p[k];
            let a = cov_ip / (var_i + eps);
            (a, mean_p[k] - a * mean_i[k])
        })
        .unzip();
    let mean_a = box_blur(&a, w, h, r);
    let mean_b = box_blur(&b, w, h, r);
    guide
        .par_iter()
        .zip(mean_a.par_iter().zip(&mean_b))
        .map(|(i, (a, b))| a * i + b)
        .collect()
}

/// Por debajo de este ruido de color (escala 0..1, ≈ 2/255) el filtro se atenúa: en fotos
/// limpias el color tiene detalle legítimo que no hay que promediar.
const CHROMA_NOISE_REFERENCE: f32 = 2.0 / 255.0;

/// [`guided_filter`] con el plano como su propia guía, mezclado en el sitio con `mix`.
///
/// Mismas cuentas que `guided_filter(p, p, …)`, pero sin duplicar lo que coincide cuando guía y
/// fuente son el mismo plano: ~4 planos temporales en lugar de ~11.
fn self_guided_in_place(plane: &mut [f32], w: usize, h: usize, r: usize, eps: f32, mix: f32) {
    let mut mean = box_blur(plane, w, h, r);
    let sq: Vec<f32> = plane.par_iter().map(|i| i * i).collect();
    let mut corr = box_blur(&sq, w, h, r);
    drop(sq);
    // a y b sustituyen a corr y mean.
    corr.par_iter_mut()
        .zip(mean.par_iter_mut())
        .for_each(|(c, m)| {
            let var = *c - *m * *m;
            let a = var / (var + eps);
            (*c, *m) = (a, *m - a * *m);
        });
    let mean_a = box_blur(&corr, w, h, r);
    drop(corr);
    let mean_b = box_blur(&mean, w, h, r);
    drop(mean);
    plane
        .par_iter_mut()
        .zip(mean_a.par_iter().zip(&mean_b))
        .for_each(|(c, (a, b))| {
            let f = a * *c + b;
            *c += (f - *c) * mix;
        });
}

/// Reduce el ruido de un plano de crominancia (Cb o Cr). `strength` en 0..1; `noise_sigma` es el
/// ruido de color estimado, que fija qué amplitud se considera ruido y cuánto se aplica.
pub fn denoise_chroma(chroma: &mut [f32], w: usize, h: usize, strength: f32, noise_sigma: f32) {
    let mix = (strength * 1.5).min(1.0) * (noise_sigma / CHROMA_NOISE_REFERENCE).clamp(0.0, 1.0);
    if mix <= 0.0 {
        return;
    }
    let r = 2 + (6.0 * strength).round() as usize;
    let e = (1.5 + 2.0 * strength) * noise_sigma;
    self_guided_in_place(chroma, w, h, r, e * e, mix);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn self_guided_matches_guided_filter() {
        let (w, h) = (53, 41);
        let src: Vec<f32> = (0..w * h)
            .map(|i| (((i * 7919) % 101) as f32 / 101.0 - 0.5) * 0.05 + ((i % w) / 20) as f32 * 0.1)
            .collect();
        let (r, eps, mix) = (4, 0.0004, 0.7);
        let reference = guided_filter(&src, &src, w, h, r, eps);
        let mut ours = src.clone();
        self_guided_in_place(&mut ours, w, h, r, eps, mix);
        for ((o, f), c) in ours.iter().zip(&reference).zip(&src) {
            assert_eq!(*o, c + (f - c) * mix);
        }
    }
}
