//! RCAS (Robust Contrast Adaptive Sharpening) de AMD FidelityFX Super Resolution 1.0.
//!
//! Port a Rust (CPU, planar f32) del algoritmo de `ffx_fsr1.h`.
//! Copyright (c) 2021 Advanced Micro Devices, Inc. Licencia MIT (ver THIRD_PARTY_NOTICES.md).
//!
//! Enfoca con un filtro en cruz cuyo peso negativo se calcula por píxel de forma que el resultado
//! nunca salga del rango local: recupera nitidez tras ampliar sin crear halos.

use rayon::prelude::*;

use crate::filters::clamp_index;
use crate::planar::Planar;

/// Límite del lóbulo negativo (como `FSR_RCAS_LIMIT`).
const LIMIT: f32 = 0.25 - 1.0 / 16.0;

/// `stops`: 0 = enfoque máximo; cada unidad lo reduce a la mitad.
pub fn rcas(src: &Planar, stops: f32) -> Planar {
    let (w, h) = (src.width, src.height);
    let con = (-stops.max(0.0)).exp2();
    let [r, g, b] = &src.channels;
    let mut out = Planar::new(w, h, src.gray, false);
    out.alpha = src.alpha.clone();
    let [or, og, ob] = &mut out.channels;
    or.par_chunks_mut(w)
        .zip(og.par_chunks_mut(w))
        .zip(ob.par_chunks_mut(w))
        .enumerate()
        .for_each(|(y, ((row_r, row_g), row_b))| {
            let up = clamp_index(y as isize - 1, h) * w;
            let mid = y * w;
            let down = clamp_index(y as isize + 1, h) * w;
            for x in 0..w {
                let left = mid + clamp_index(x as isize - 1, w);
                let right = mid + clamp_index(x as isize + 1, w);
                let taps = [up + x, left, mid + x, right, down + x];
                let read = |p: &[f32]| taps.map(|i| p[i].clamp(0.0, 1.0));
                let (cr, cg, cb) = (read(r), read(g), read(b));

                // Lóbulo por canal: el mayor peso negativo que no sobrepasa el rango local.
                let lobe_of = |[b_, d, e, f, h_]: [f32; 5]| {
                    let mn4 = b_.min(d).min(f).min(h_);
                    let mx4 = b_.max(d).max(f).max(h_);
                    let hit_min = mn4.min(e) / (4.0 * mx4).max(1e-6);
                    let hit_max = (1.0 - mx4.max(e)) / (4.0 * mn4 - 4.0).min(-1e-6);
                    (-hit_min).max(hit_max)
                };
                let lobe = lobe_of(cr)
                    .max(lobe_of(cg))
                    .max(lobe_of(cb))
                    .clamp(-LIMIT, 0.0)
                    * con;
                let rcp = (4.0 * lobe + 1.0).recip();
                let apply = |[b_, d, e, f, h_]: [f32; 5]| (lobe * (b_ + d + f + h_) + e) * rcp;
                row_r[x] = apply(cr);
                row_g[x] = apply(cg);
                row_b[x] = apply(cb);
            }
        });
    out
}
