//! Filtros básicos sobre un plano `f32` (ancho × alto), con bordes replicados.

use rayon::prelude::*;

/// Filas por banda al paralelizar pasadas verticales.
const BAND: usize = 64;

#[inline]
pub(crate) fn clamp_index(i: isize, len: usize) -> usize {
    i.clamp(0, len as isize - 1) as usize
}

/// Media en una ventana (2r+1)×(2r+1). Coste O(1) por píxel gracias a sumas deslizantes.
///
/// Las sumas se llevan en `f64`: la de unos pocos valores `f32` es exacta, así el resultado no
/// depende de dónde empieza la fila o la banda (una zona recortada da lo mismo que la foto).
pub fn box_blur(src: &[f32], w: usize, h: usize, r: usize) -> Vec<f32> {
    if r == 0 || w == 0 || h == 0 {
        return src.to_vec();
    }
    let norm = 1.0 / (2 * r + 1) as f64;

    // Pasada horizontal: cada fila es independiente.
    let mut horiz = vec![0.0f32; w * h];
    horiz
        .par_chunks_mut(w)
        .zip(src.par_chunks(w))
        .for_each(|(dst, row)| {
            let ri = r as isize;
            let mut acc: f64 = (-ri..=ri).map(|k| f64::from(row[clamp_index(k, w)])).sum();
            for x in 0..w {
                dst[x] = (acc * norm) as f32;
                let add = row[clamp_index(x as isize + ri + 1, w)];
                let sub = row[clamp_index(x as isize - ri, w)];
                acc += f64::from(add) - f64::from(sub);
            }
        });

    // Pasada vertical por bandas de filas: cada banda inicializa su acumulador y desliza.
    let mut out = vec![0.0f32; w * h];
    out.par_chunks_mut(w * BAND)
        .enumerate()
        .for_each(|(band, dst)| {
            let y0 = band * BAND;
            let rows = dst.len() / w;
            let ri = r as isize;
            let mut acc = vec![0.0f64; w];
            for k in -ri..=ri {
                let row = &horiz[clamp_index(y0 as isize + k, h) * w..][..w];
                acc.iter_mut()
                    .zip(row)
                    .for_each(|(a, v)| *a += f64::from(*v));
            }
            for dy in 0..rows {
                let y = (y0 + dy) as isize;
                dst[dy * w..(dy + 1) * w]
                    .iter_mut()
                    .zip(&acc)
                    .for_each(|(d, a)| *d = (a * norm) as f32);
                let add = &horiz[clamp_index(y + ri + 1, h) * w..][..w];
                let sub = &horiz[clamp_index(y - ri, h) * w..][..w];
                acc.iter_mut()
                    .zip(add.iter().zip(sub))
                    .for_each(|(a, (p, m))| *a += f64::from(*p) - f64::from(*m));
            }
        });
    out
}

/// Desenfoque gaussiano aproximado con tres pasadas de caja (error < 3 % frente al exacto).
pub fn gaussian_blur(src: &[f32], w: usize, h: usize, sigma: f32) -> Vec<f32> {
    if sigma <= 0.0 {
        return src.to_vec();
    }
    // Radio de caja equivalente para 3 pasadas: varianza de una caja de ancho n es (n²-1)/12.
    let n = ((12.0 * sigma * sigma / 3.0) + 1.0).sqrt();
    let r = ((n - 1.0) / 2.0).round().max(1.0) as usize;
    let once = box_blur(src, w, h, r);
    let twice = box_blur(&once, w, h, r);
    drop(once);
    box_blur(&twice, w, h, r)
}

/// Traspone un plano (útil para reutilizar un filtro horizontal en vertical).
pub fn transpose(src: &[f32], w: usize, h: usize) -> Vec<f32> {
    let mut out = vec![0.0f32; w * h];
    out.par_chunks_mut(h).enumerate().for_each(|(x, col)| {
        for (y, v) in col.iter_mut().enumerate() {
            *v = src[y * w + x];
        }
    });
    out
}

/// Mínimo y máximo de la vecindad 3×3 de cada píxel.
pub fn min_max_3x3(src: &[f32], w: usize, h: usize) -> (Vec<f32>, Vec<f32>) {
    let mut lo = vec![0.0f32; w * h];
    let mut hi = vec![0.0f32; w * h];
    lo.par_chunks_mut(w)
        .zip(hi.par_chunks_mut(w))
        .enumerate()
        .for_each(|(y, (lo_row, hi_row))| {
            for x in 0..w {
                let mut mn = f32::INFINITY;
                let mut mx = f32::NEG_INFINITY;
                for dy in -1..=1isize {
                    let yy = clamp_index(y as isize + dy, h);
                    for dx in -1..=1isize {
                        let v = src[yy * w + clamp_index(x as isize + dx, w)];
                        mn = mn.min(v);
                        mx = mx.max(v);
                    }
                }
                lo_row[x] = mn;
                hi_row[x] = mx;
            }
        });
    (lo, hi)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn box_blur_keeps_constant_and_mean() {
        let (w, h) = (37, 150);
        let src = vec![0.25f32; w * h];
        let out = box_blur(&src, w, h, 3);
        assert!(out.iter().all(|v| (v - 0.25).abs() < 1e-5));

        let ramp: Vec<f32> = (0..w * h).map(|i| (i % w) as f32 / w as f32).collect();
        let out = box_blur(&ramp, w, h, 2);
        // En el interior una rampa lineal no cambia con una media simétrica.
        for y in 0..h {
            for x in 3..w - 3 {
                assert!((out[y * w + x] - ramp[y * w + x]).abs() < 1e-4);
            }
        }
    }

    #[test]
    fn transpose_roundtrip() {
        let (w, h) = (5, 3);
        let src: Vec<f32> = (0..w * h).map(|i| i as f32).collect();
        let t = transpose(&src, w, h);
        assert_eq!(t[1], src[w]); // (x=0,y=1) → fila 0, columna 1
        assert_eq!(transpose(&t, h, w), src);
    }
}
