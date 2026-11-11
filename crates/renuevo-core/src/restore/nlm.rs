//! Non-Local Means rápido (Buades et al. 2005; aceleración por sumas de Darbon et al. 2008).
//!
//! Cada píxel se reemplaza por una media ponderada de píxeles cercanos cuyo *entorno* (parche)
//! se parece al suyo. Para cada desplazamiento de búsqueda se calcula la distancia de todos los
//! parches a la vez con sumas de caja, así el coste no depende del tamaño del parche.
//! Se procesa por bandas de filas en paralelo para aprovechar la caché.

use rayon::prelude::*;

use crate::filters::clamp_index;
use crate::{Progress, Result};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NlmParams {
    /// Parámetro de filtrado (escala 0..1). Mayor = más suavizado.
    pub h: f32,
    /// σ del ruido estimado: las distancias por debajo de 2σ² se consideran "iguales".
    pub sigma: f32,
    /// Radio de la ventana de búsqueda (5 → 11×11 desplazamientos).
    pub search_radius: usize,
    /// Radio del parche (2 → 5×5).
    pub patch_radius: usize,
}

const BAND: usize = 32;

/// e^(-x) para x ≥ 0 con (1 - x/256)^256: sin llamadas a libm y vectorizable.
#[inline]
fn exp_neg(x: f32) -> f32 {
    let mut t = (1.0 - x * (1.0 / 256.0)).max(0.0);
    for _ in 0..8 {
        t *= t;
    }
    t
}

pub fn nlm(
    src: &[f32],
    w: usize,
    h: usize,
    params: NlmParams,
    progress: &Progress,
) -> Result<Vec<f32>> {
    let s = params.search_radius as isize;
    let p = params.patch_radius;
    let pi = p as isize;
    let inv_patch_area = 1.0 / ((2 * p + 1) * (2 * p + 1)) as f32;
    let inv_h2 = 1.0 / (params.h * params.h).max(1e-12);
    let bias = 2.0 * params.sigma * params.sigma;

    let bands = h.div_ceil(BAND);
    progress.begin("Reduciendo ruido (NLM)", bands as u64);

    let mut out = vec![0.0f32; w * h];
    out.par_chunks_mut(w * BAND)
        .enumerate()
        .try_for_each(|(band, dst)| -> Result<()> {
            progress.check()?;
            let y0 = band * BAND;
            let rows = dst.len() / w;
            let ext_rows = rows + 2 * p;
            let ext0 = y0 as isize - pi;

            let mut num = vec![0.0f32; rows * w];
            let mut den = vec![0.0f32; rows * w];
            let mut diff = vec![0.0f32; ext_rows * w];
            let mut hsum = vec![0.0f32; ext_rows * w];

            for dy in -s..=s {
                for dx in -s..=s {
                    if dx == 0 && dy == 0 {
                        continue;
                    }
                    // 1) Diferencias al cuadrado entre la imagen y su versión desplazada.
                    for r in 0..ext_rows {
                        let y = clamp_index(ext0 + r as isize, h);
                        let y2 = clamp_index(y as isize + dy, h);
                        let (a, b) = (&src[y * w..(y + 1) * w], &src[y2 * w..(y2 + 1) * w]);
                        let d = &mut diff[r * w..(r + 1) * w];
                        for x in 0..w {
                            let v = a[x] - b[clamp_index(x as isize + dx, w)];
                            d[x] = v * v;
                        }
                    }
                    // 2) Suma horizontal de ventana (2p+1).
                    for r in 0..ext_rows {
                        let row = &diff[r * w..(r + 1) * w];
                        let out_row = &mut hsum[r * w..(r + 1) * w];
                        let mut acc: f32 = (-pi..=pi).map(|k| row[clamp_index(k, w)]).sum();
                        for x in 0..w {
                            out_row[x] = acc;
                            acc += row[clamp_index(x as isize + pi + 1, w)]
                                - row[clamp_index(x as isize - pi, w)];
                        }
                    }
                    // 3) Suma vertical, peso y acumulación.
                    for r in 0..rows {
                        let y = y0 + r;
                        let y2 = clamp_index(y as isize + dy, h);
                        let shifted = &src[y2 * w..(y2 + 1) * w];
                        let base = r * w;
                        for x in 0..w {
                            let mut dist = 0.0f32;
                            for k in 0..=2 * p {
                                dist += hsum[(r + k) * w + x];
                            }
                            let d2 = (dist * inv_patch_area - bias).max(0.0);
                            let wt = exp_neg(d2 * inv_h2);
                            num[base + x] += wt * shifted[clamp_index(x as isize + dx, w)];
                            den[base + x] += wt;
                        }
                    }
                }
            }
            // El píxel central pesa 1 (distancia 0). No usamos el truco de «peso del mejor
            // vecino»: al normalizar, un vecino muy distinto con peso ínfimo acabaría mezclado
            // al 50 % y emborronaría las fotos limpias.
            for r in 0..rows {
                let y = y0 + r;
                for x in 0..w {
                    let k = r * w + x;
                    dst[k] = (num[k] + src[y * w + x]) / (den[k] + 1.0);
                }
            }
            progress.advance(1);
            Ok(())
        })?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::exp_neg;

    #[test]
    fn exp_neg_is_close_to_libm() {
        for i in 0..100 {
            let x = i as f32 * 0.05;
            let rel = (exp_neg(x) - (-x).exp()).abs();
            assert!(rel < 0.004, "x={x}: {} vs {}", exp_neg(x), (-x).exp());
        }
    }
}
