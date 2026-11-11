//! EASU (Edge Adaptive Spatial Upsampling) de AMD FidelityFX Super Resolution 1.0.
//!
//! Port a Rust (CPU, planar f32) del algoritmo de `ffx_fsr1.h`.
//! Copyright (c) 2021 Advanced Micro Devices, Inc. Licencia MIT (ver THIRD_PARTY_NOTICES.md).
//!
//! Para cada píxel de salida analiza la luminancia de los 12 vecinos más cercanos, estima la
//! dirección y la intensidad del borde, y aplica un núcleo tipo Lanczos2 estirado a lo largo del
//! borde. Así los bordes diagonales quedan limpios en lugar de escalonados. El resultado se limita
//! al mínimo/máximo de los 4 vecinos más cercanos para no crear halos.

use rayon::prelude::*;

use crate::filters::clamp_index;
use crate::planar::{Planar, rows_mut};
use crate::{Progress, Result};

/// Posiciones de los 12 taps relativas a la esquina superior izquierda del cuadrado 2×2:
/// ```text
///     b c
///   e f g h
///   i j k l
///     n o
/// ```
const TAPS: [(isize, isize); 12] = [
    (0, -1), // b
    (1, -1), // c
    (-1, 0), // e
    (0, 0),  // f
    (1, 0),  // g
    (2, 0),  // h
    (-1, 1), // i
    (0, 1),  // j
    (1, 1),  // k
    (2, 1),  // l
    (0, 2),  // n
    (1, 2),  // o
];
const B: usize = 0;
const C: usize = 1;
const E: usize = 2;
const F: usize = 3;
const G: usize = 4;
const H: usize = 5;
const I: usize = 6;
const J: usize = 7;
const K: usize = 8;
const L: usize = 9;
const N: usize = 10;
const O: usize = 11;

/// Acumula dirección y "cantidad de borde" de una cruz:
/// ```text
///     a
///   b c d
///     e
/// ```
#[inline]
fn accumulate(dir: &mut (f32, f32), len: &mut f32, w: f32, [la, lb, lc, ld, le]: [f32; 5]) {
    let axis = |dir_v: f32, d1: f32, d2: f32| {
        let m = d1.abs().max(d2.abs());
        let l = if m > 0.0 {
            (dir_v.abs() / m).min(1.0)
        } else {
            0.0
        };
        l * l
    };
    let dir_x = ld - lb;
    dir.0 += dir_x * w;
    *len += axis(dir_x, ld - lc, lc - lb) * w;
    let dir_y = le - la;
    dir.1 += dir_y * w;
    *len += axis(dir_y, le - lc, lc - la) * w;
}

/// Una pasada de EASU de `src` a `nw`×`nh` (pensada para factores ≤ 2).
/// `scale` = píxeles de entrada por píxel de salida (1/factor exacto, no el cociente de tamaños
/// redondeados: así una zona recortada cae exactamente sobre la misma rejilla que la foto).
///
/// Todos los planos (también el alfa) se filtran con los mismos pesos, calculados sobre la
/// luminancia del color.
///
/// Si `src` es un recorte de una imagen mayor, `origin` = ((x, y) del recorte en la entrada
/// completa, (x, y) de su resultado en la salida completa): las posiciones se calculan en
/// coordenadas de la imagen completa y el recorte da exactamente los mismos valores.
pub fn easu_pass(
    src: &Planar,
    nw: usize,
    nh: usize,
    scale: f32,
    origin: ((usize, usize), (usize, usize)),
    progress: &Progress,
) -> Result<Planar> {
    let ((in_x, in_y), (out_x, out_y)) = origin;
    let (w, h) = (src.width, src.height);
    // Luma aproximada de FSR (rango 0..2), solo para analizar bordes. En gris es 2·v: se lee
    // del propio plano sin copiarlo.
    let computed: Vec<f32>;
    let (luma, luma_scale): (&[f32], f32) = match &src.channels[..] {
        [r, g, b] => {
            computed = r
                .par_iter()
                .zip(g.par_iter().zip(b))
                .map(|(r, (g, b))| 0.5 * r + g + 0.5 * b)
                .collect();
            (&computed, 1.0)
        }
        planes => (&planes[0], 2.0),
    };
    let planes: Vec<&[f32]> = src
        .channels
        .iter()
        .chain(src.alpha.as_ref())
        .map(Vec::as_slice)
        .collect();

    let (scale_x, scale_y) = (scale, scale);
    let off_x = 0.5 * scale_x - 0.5;
    let off_y = 0.5 * scale_y - 0.5;

    let mut out = Planar::new(nw, nh, src.is_gray(), src.alpha.is_some());
    out.premultiplied = src.premultiplied;
    let out_planes = out
        .channels
        .iter_mut()
        .chain(out.alpha.as_mut())
        .map(Vec::as_mut_slice);
    rows_mut(out_planes, nw, nh)
        .into_par_iter()
        .enumerate()
        .try_for_each(|(oy, mut rows)| -> Result<()> {
            if oy % 64 == 0 {
                progress.check()?;
            }
            let py = (out_y + oy) as f32 * scale_y + off_y;
            let fy = py.floor();
            let ty = py - fy;
            let fy = fy as isize - in_y as isize;
            for ox in 0..nw {
                let px = (out_x + ox) as f32 * scale_x + off_x;
                let fx = px.floor();
                let tx = px - fx;
                let fx = fx as isize - in_x as isize;

                let mut idx = [0usize; 12];
                let mut l = [0.0f32; 12];
                for (t, (dx, dy)) in TAPS.iter().enumerate() {
                    let i = clamp_index(fy + dy, h) * w + clamp_index(fx + dx, w);
                    idx[t] = i;
                    l[t] = luma[i] * luma_scale;
                }

                // Dirección y longitud del borde, ponderadas bilinealmente en el cuadrado 2×2.
                let mut dir = (0.0f32, 0.0f32);
                let mut len = 0.0f32;
                accumulate(
                    &mut dir,
                    &mut len,
                    (1.0 - tx) * (1.0 - ty),
                    [l[B], l[E], l[F], l[G], l[J]],
                );
                accumulate(
                    &mut dir,
                    &mut len,
                    tx * (1.0 - ty),
                    [l[C], l[F], l[G], l[H], l[K]],
                );
                accumulate(
                    &mut dir,
                    &mut len,
                    (1.0 - tx) * ty,
                    [l[F], l[I], l[J], l[K], l[N]],
                );
                accumulate(&mut dir, &mut len, tx * ty, [l[G], l[J], l[K], l[L], l[O]]);

                let dir2 = dir.0 * dir.0 + dir.1 * dir.1;
                let (dx, dy) = if dir2 < 1.0 / 32768.0 {
                    (1.0, dir.1)
                } else {
                    let inv = dir2.sqrt().recip();
                    (dir.0 * inv, dir.1 * inv)
                };
                let len = {
                    let l = len * 0.5;
                    l * l
                };
                // Estirar el núcleo: 1 en horizontal/vertical, √2 en diagonal.
                let stretch = (dx * dx + dy * dy) / dx.abs().max(dy.abs()).max(1e-6);
                let len2 = (1.0 + (stretch - 1.0) * len, 1.0 - 0.5 * len);
                // La ventana pasa de ±√2 a algo más de ±2 según cuánto borde haya.
                let lob = 0.5 + ((1.0 / 4.0 - 0.04) - 0.5) * len;
                let clp = lob.recip();

                let mut weights = [0.0f32; 12];
                let mut acc_w = 0.0f32;
                for (t, (tx_off, ty_off)) in TAPS.iter().enumerate() {
                    let ox_ = *tx_off as f32 - tx;
                    let oy_ = *ty_off as f32 - ty;
                    // Rotar el desplazamiento según la dirección y aplicar la anisotropía.
                    let vx = (ox_ * dx + oy_ * dy) * len2.0;
                    let vy = (ox_ * -dy + oy_ * dx) * len2.1;
                    let d2 = (vx * vx + vy * vy).min(clp);
                    // Aproximación de Lanczos2 sin sin()/sqrt():
                    // (25/16·(2/5·x² − 1)² − (25/16 − 1)) · (lob·x² − 1)²
                    let wb = 2.0 / 5.0 * d2 - 1.0;
                    let wa = lob * d2 - 1.0;
                    let wt = (25.0 / 16.0 * wb * wb - (25.0 / 16.0 - 1.0)) * (wa * wa);
                    weights[t] = wt;
                    acc_w += wt;
                }
                let inv_w = if acc_w.abs() > 1e-12 {
                    acc_w.recip()
                } else {
                    0.0
                };
                // Anti-halo: limitar al rango de los 4 vecinos más cercanos.
                let near = [idx[F], idx[G], idx[J], idx[K]];
                for (plane, row) in planes.iter().zip(rows.iter_mut()) {
                    if inv_w == 0.0 {
                        row[ox] = plane[idx[F]];
                        continue;
                    }
                    let mut acc = 0.0f32;
                    for (&i, wt) in idx.iter().zip(weights) {
                        acc += plane[i] * wt;
                    }
                    let (mut mn, mut mx) = (f32::INFINITY, f32::NEG_INFINITY);
                    for &i in &near {
                        mn = mn.min(plane[i]);
                        mx = mx.max(plane[i]);
                    }
                    row[ox] = (acc * inv_w).max(mn).min(mx);
                }
            }
            progress.advance(1);
            Ok(())
        })?;
    Ok(out)
}
