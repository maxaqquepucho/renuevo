//! RCAS (Robust Contrast Adaptive Sharpening) de AMD FidelityFX Super Resolution 1.0.
//!
//! Port a Rust (CPU, planar f32) del algoritmo de `ffx_fsr1.h`.
//! Copyright (c) 2021 Advanced Micro Devices, Inc. Licencia MIT (ver THIRD_PARTY_NOTICES.md).
//!
//! Enfoca con un filtro en cruz cuyo peso negativo se calcula por píxel de forma que el resultado
//! nunca salga del rango local: recupera nitidez tras ampliar sin crear halos.

use rayon::prelude::*;

use crate::planar::Planar;

/// Límite del lóbulo negativo (como `FSR_RCAS_LIMIT`).
const LIMIT: f32 = 0.25 - 1.0 / 16.0;

/// Filas por banda: cada banda se enfoca en el sitio en paralelo con las demás.
const BAND: usize = 128;

/// Una fila de cada plano.
type Rows = Vec<Vec<f32>>;

/// Enfoca `p` en el sitio (sin copia de la imagen). `stops`: 0 = enfoque máximo; cada unidad lo
/// reduce a la mitad.
///
/// El peso se decide con los canales de color; el alfa, si lo hay, se enfoca con el mismo peso
/// para que el color premultiplicado y el alfa sigan casando.
pub fn rcas(p: &mut Planar, stops: f32) {
    let (w, h) = (p.width, p.height);
    if p.is_empty() {
        return;
    }
    let con = (-stops.max(0.0)).exp2();
    let color = p.channels.len();
    let mut planes: Vec<&mut [f32]> = p
        .channels
        .iter_mut()
        .chain(p.alpha.as_mut())
        .map(Vec::as_mut_slice)
        .collect();

    // Cada banda necesita la fila original de encima y la de debajo, que pertenecen a las
    // bandas vecinas y habrán cambiado: se copian antes.
    let bands = h.div_ceil(BAND);
    let row = |plane: &[f32], y: usize| plane[y * w..(y + 1) * w].to_vec();
    let borders: Vec<(Option<Rows>, Option<Rows>)> = (0..bands)
        .map(|b| {
            let (y0, y1) = (b * BAND, ((b + 1) * BAND).min(h));
            let above = (y0 > 0).then(|| planes.iter().map(|pl| row(pl, y0 - 1)).collect());
            let below = (y1 < h).then(|| planes.iter().map(|pl| row(pl, y1)).collect());
            (above, below)
        })
        .collect();

    // Bandas: para cada una, sus filas de cada plano.
    let mut band_planes: Vec<Vec<&mut [f32]>> = (0..bands).map(|_| Vec::new()).collect();
    for plane in planes.iter_mut() {
        for (b, chunk) in plane.chunks_mut(BAND * w).enumerate() {
            band_planes[b].push(chunk);
        }
    }

    band_planes
        .into_par_iter()
        .zip(borders)
        .enumerate()
        .for_each(|(b, (mut band, (above, below)))| {
            let (y0, y1) = (b * BAND, ((b + 1) * BAND).min(h));
            let n = band.len();
            // Filas originales anterior, actual y siguiente de cada plano, con el primer y el
            // último píxel repetidos a cada lado: se leen x−1 y x+1 sin comprobar bordes.
            let pad = |dst: &mut [f32], row: &[f32]| {
                dst[1..=w].copy_from_slice(row);
                (dst[0], dst[w + 1]) = (row[0], row[w - 1]);
            };
            let mut prev: Rows = vec![vec![0.0; w + 2]; n];
            let mut cur: Rows = vec![vec![0.0; w + 2]; n];
            let mut next: Rows = vec![vec![0.0; w + 2]; n];
            for p in 0..n {
                pad(&mut cur[p], &band[p][..w]);
                match &above {
                    Some(above) => pad(&mut prev[p], &above[p]),
                    None => prev[p].copy_from_slice(&cur[p]),
                }
            }
            let (mut lobe, mut rcp) = (vec![0.0f32; w], vec![0.0f32; w]);
            for y in y0..y1 {
                let local = y - y0;
                // Fila original siguiente: aún sin tocar dentro de la banda, guardada si es de
                // la banda de abajo, o la misma en el borde inferior de la foto.
                for p in 0..n {
                    if y + 1 < y1 {
                        pad(&mut next[p], &band[p][(local + 1) * w..(local + 2) * w]);
                    } else if let Some(below) = &below {
                        pad(&mut next[p], &below[p]);
                    } else {
                        let (c, nx) = (&cur[p], &mut next[p]);
                        nx.copy_from_slice(c);
                    }
                }
                // Lóbulo: el mayor peso negativo que ningún canal de color sobrepasa.
                lobe.fill(f32::NEG_INFINITY);
                for p in 0..color {
                    for (l, t) in lobe.iter_mut().zip(taps(&prev[p], &cur[p], &next[p])) {
                        *l = l.max(lobe_of(t));
                    }
                }
                for (l, r) in lobe.iter_mut().zip(rcp.iter_mut()) {
                    *l = l.clamp(-LIMIT, 0.0) * con;
                    *r = (4.0 * *l + 1.0).recip();
                }
                for p in 0..n {
                    let out = &mut band[p][local * w..(local + 1) * w];
                    let t = taps(&prev[p], &cur[p], &next[p]);
                    for ((o, [b_, d, e, f, h_]), (l, r)) in
                        out.iter_mut().zip(t).zip(lobe.iter().zip(&rcp))
                    {
                        *o = (l * (b_ + d + f + h_) + e) * r;
                    }
                }
                std::mem::swap(&mut prev, &mut cur);
                std::mem::swap(&mut cur, &mut next);
            }
        });
}

/// Cruz de cada píxel de una fila (arriba, izquierda, centro, derecha, abajo) recortada a 0..1,
/// a partir de las filas con borde repetido.
fn taps<'a>(
    prev: &'a [f32],
    cur: &'a [f32],
    next: &'a [f32],
) -> impl Iterator<Item = [f32; 5]> + 'a {
    prev[1..]
        .iter()
        .zip(cur.windows(3))
        .zip(&next[1..])
        .map(|((u, c), d)| [*u, c[0], c[1], c[2], *d].map(|v| v.clamp(0.0, 1.0)))
}

/// Lóbulo de un canal: el mayor peso negativo que no sobrepasa su rango local.
#[inline]
fn lobe_of([b_, d, e, f, h_]: [f32; 5]) -> f32 {
    let mn4 = b_.min(d).min(f).min(h_);
    let mx4 = b_.max(d).max(f).max(h_);
    let hit_min = mn4.min(e) / (4.0 * mx4).max(1e-6);
    let hit_max = (1.0 - mx4.max(e)) / (4.0 * mn4 - 4.0).min(-1e-6);
    (-hit_min).max(hit_max)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filters::clamp_index;

    /// RCAS directo, píxel a píxel y sin bandas (como el original, a otra imagen).
    fn reference(src: &Planar, stops: f32) -> Vec<Vec<f32>> {
        let (w, h) = (src.width, src.height);
        let con = (-stops.max(0.0)).exp2();
        let planes: Vec<&Vec<f32>> = src.channels.iter().chain(src.alpha.as_ref()).collect();
        let mut out = vec![vec![0.0; w * h]; planes.len()];
        for y in 0..h {
            let (up, down) = (
                clamp_index(y as isize - 1, h),
                clamp_index(y as isize + 1, h),
            );
            for x in 0..w {
                let (l, r) = (
                    clamp_index(x as isize - 1, w),
                    clamp_index(x as isize + 1, w),
                );
                let taps = |p: &[f32]| {
                    [
                        p[up * w + x],
                        p[y * w + l],
                        p[y * w + x],
                        p[y * w + r],
                        p[down * w + x],
                    ]
                    .map(|v| v.clamp(0.0, 1.0))
                };
                let lobe = planes[..src.channels.len()]
                    .iter()
                    .map(|p| lobe_of(taps(p)))
                    .fold(f32::NEG_INFINITY, f32::max)
                    .clamp(-LIMIT, 0.0)
                    * con;
                let rcp = (4.0 * lobe + 1.0).recip();
                for (o, p) in out.iter_mut().zip(&planes) {
                    let [b, d, e, f, hh] = taps(p);
                    o[y * w + x] = (lobe * (b + d + f + hh) + e) * rcp;
                }
            }
        }
        out
    }

    /// Las bandas en el sitio dan exactamente lo mismo que el RCAS directo, también en sus
    /// fronteras, en gris y con alfa.
    #[test]
    fn in_place_bands_match_direct_rcas() {
        for (w, h, gray, alpha) in [
            (37, 300, false, true),
            (5, 129, true, false),
            (1, 1, false, false),
        ] {
            let mut p = Planar::new(w, h, gray, alpha);
            for (k, plane) in p.channels.iter_mut().chain(p.alpha.as_mut()).enumerate() {
                for (i, v) in plane.iter_mut().enumerate() {
                    *v = ((i * 7919 + k * 104_729) % 1000) as f32 / 999.0;
                }
            }
            let expected = reference(&p, 0.4);
            rcas(&mut p, 0.4);
            let got: Vec<&Vec<f32>> = p.channels.iter().chain(p.alpha.as_ref()).collect();
            for (g, e) in got.iter().zip(&expected) {
                assert_eq!(*g, e, "{w}×{h}");
            }
        }
    }
}
