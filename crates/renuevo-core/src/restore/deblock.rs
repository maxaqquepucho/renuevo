//! Reducción de bloques JPEG.
//!
//! 1. Detecta la rejilla de 8×8 (puede estar desplazada si la foto se giró o recortó). Se mide
//!    una vez sobre la foto completa, junto con el ruido ([`super::estimate_noise`]): una zona
//!    recortada puede no tener bloqueo suficiente para detectarla y la vista previa no
//!    coincidiría con el resultado.
//! 2. En cada frontera de bloque, si el salto es pequeño y ambos lados son planos (señal de
//!    artefacto, no de borde real), lo reparte suavemente entre los píxeles vecinos, al estilo
//!    del filtro de bucle de H.264.

use rayon::prelude::*;

const BLOCK: usize = 8;

/// Rejilla de un plano: desfase (0..8) de las fronteras verticales (`x`) y horizontales (`y`),
/// o `None` si en esa dirección no hay bloqueo claro.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PlaneGrid {
    pub x: Option<u8>,
    pub y: Option<u8>,
}

/// Rejilla de bloques de cada plano (Y, Cb, Cr; en gris solo el primero), medida sobre la foto
/// completa.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BlockGrid {
    pub planes: [PlaneGrid; 3],
}

impl BlockGrid {
    /// La misma rejilla vista desde un recorte que empieza en (`x0`, `y0`) de la foto.
    pub fn cropped(self, x0: usize, y0: usize) -> Self {
        let shift = |offset: Option<u8>, origin: usize| {
            offset.map(|o| ((usize::from(o) + BLOCK - origin % BLOCK) % BLOCK) as u8)
        };
        Self {
            planes: self.planes.map(|p| PlaneGrid {
                x: shift(p.x, x0),
                y: shift(p.y, y0),
            }),
        }
    }
}

/// Desfase con más salto medio, si destaca claramente sobre el resto. `energy[k]` es la suma de
/// saltos en las posiciones `1..len` con resto `k` de `lines` líneas (filas o columnas).
/// La mide [`super::noise`] en la misma pasada que el ruido.
pub(crate) fn pick_offset(energy: &[f64; BLOCK], len: usize, lines: usize) -> Option<u8> {
    let means: Vec<f64> = energy
        .iter()
        .enumerate()
        .map(|(k, e)| {
            let count = (1..len).filter(|i| i % BLOCK == k).count() * lines;
            e / count.max(1) as f64
        })
        .collect();
    let (best, &peak) = means.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1))?;
    let others = (means.iter().sum::<f64>() - peak) / (BLOCK - 1) as f64;
    // Con bloqueo real, la frontera tiene claramente más salto que el interior de los bloques.
    (peak > others * 1.08 && peak > 1e-4).then_some(best as u8)
}

/// Umbrales del filtro: salto máximo que consideramos artefacto, planitud exigida a cada lado
/// y corrección máxima.
fn thresholds(strength: f32) -> (f32, f32, f32) {
    let alpha = 0.10 * strength;
    (alpha, 0.03 * strength, alpha * 0.5)
}

/// Filtra una frontera entre `p0` y `q0` con tres píxeles a cada lado (`p2 p1 p0 | q0 q1 q2`).
/// Devuelve los nuevos `p1 p0 q0 q1`, o `None` si no parece un artefacto.
#[inline]
fn filter_edge([p2, p1, p0, q0, q1, q2]: [f32; 6], strength: f32) -> Option<[f32; 4]> {
    let (alpha, beta, tc) = thresholds(strength);
    if !((q0 - p0).abs() < alpha && (p1 - p0).abs() < beta && (q1 - q0).abs() < beta) {
        return None;
    }
    let delta = ((4.0 * (q0 - p0) + (p1 - q1)) / 8.0).clamp(-tc, tc);
    Some([
        if (p2 - p0).abs() < beta {
            p1 + delta * 0.5
        } else {
            p1
        },
        p0 + delta,
        q0 - delta,
        if (q2 - q0).abs() < beta {
            q1 - delta * 0.5
        } else {
            q1
        },
    ])
}

/// Primera frontera filtrable (hacen falta 3 píxeles a cada lado) con resto `offset`.
fn first_edge(offset: usize) -> usize {
    if offset < 3 { offset + BLOCK } else { offset }
}

/// Suaviza las fronteras verticales (entre x-1 y x, con x ≡ offset mod 8) de cada fila.
fn filter_vertical_edges(plane: &mut [f32], w: usize, offset: usize, strength: f32) {
    plane.par_chunks_mut(w).for_each(|row| {
        let mut x = first_edge(offset);
        while x + 2 < w {
            let px = [
                row[x - 3],
                row[x - 2],
                row[x - 1],
                row[x],
                row[x + 1],
                row[x + 2],
            ];
            if let Some([p1, p0, q0, q1]) = filter_edge(px, strength) {
                row[x - 2..x + 2].copy_from_slice(&[p1, p0, q0, q1]);
            }
            x += BLOCK;
        }
    });
}

/// Suaviza las fronteras horizontales (entre y-1 e y, con y ≡ offset mod 8), sin trasponer:
/// el plano se reparte en tiras de 8 filas centradas en cada frontera, y cada tira solo lee y
/// escribe sus propias filas.
fn filter_horizontal_edges(plane: &mut [f32], w: usize, h: usize, offset: usize, strength: f32) {
    let start = ((offset + BLOCK / 2) % BLOCK).min(h);
    let (head, rest) = plane.split_at_mut(start * w);
    let strips: Vec<(usize, &mut [f32])> = std::iter::once((0, head))
        .chain(
            rest.chunks_mut(BLOCK * w)
                .enumerate()
                .map(|(i, s)| (start + i * BLOCK, s)),
        )
        .collect();
    strips.into_par_iter().for_each(|(y0, strip)| {
        let rows = strip.len() / w;
        for y in y0..y0 + rows {
            if y % BLOCK != offset || y < first_edge(offset) || y + 2 >= h || y < y0 + 3 {
                continue;
            }
            if y + 2 >= y0 + rows {
                continue;
            }
            let r = |dy: usize| (y - y0 + dy - 3) * w;
            for x in 0..w {
                let px = [0, 1, 2, 3, 4, 5].map(|dy| strip[r(dy) + x]);
                if let Some(new) = filter_edge(px, strength) {
                    for (dy, v) in (1..5).zip(new) {
                        strip[r(dy) + x] = v;
                    }
                }
            }
        }
    });
}

/// Aplica el deblocking a un plano con la rejilla ya medida. Devuelve si trató algo.
pub fn deblock(plane: &mut [f32], w: usize, h: usize, strength: f32, grid: PlaneGrid) -> bool {
    if strength <= 0.0 || w == 0 {
        return false;
    }
    if let Some(ox) = grid.x {
        filter_vertical_edges(plane, w, usize::from(ox), strength);
    }
    if let Some(oy) = grid.y {
        filter_horizontal_edges(plane, w, h, usize::from(oy), strength);
    }
    grid.x.is_some() || grid.y.is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filters::transpose;

    /// El filtro de fronteras horizontales en el sitio da lo mismo que trasponer, filtrar como
    /// verticales y deshacer (lo que se hacía antes).
    #[test]
    fn horizontal_edges_match_transposed_filter() {
        let (w, h) = (37, 61);
        let src: Vec<f32> = (0..w * h)
            .map(|i| {
                let (x, y) = (i % w, i / w);
                0.4 + 0.02 * ((y + 5) / 8) as f32 + 0.003 * ((x * 7 + y * 3) % 5) as f32
            })
            .collect();
        for offset in 0..BLOCK {
            let mut direct = src.clone();
            filter_horizontal_edges(&mut direct, w, h, offset, 1.0);
            let mut t = transpose(&src, w, h);
            filter_vertical_edges(&mut t, h, offset, 1.0);
            assert_eq!(direct, transpose(&t, h, w), "desfase {offset}");
            assert_ne!(direct, src, "desfase {offset}: no filtró nada");
        }
    }

    #[test]
    fn cropped_grid_keeps_global_offsets() {
        let grid = BlockGrid {
            planes: [PlaneGrid {
                x: Some(3),
                y: Some(0),
            }; 3],
        };
        let c = grid.cropped(13, 22).planes[0];
        // Frontera global en x = 3 + 8k → en el recorte, x − 13 ≡ 6; y = 8k → y − 22 ≡ 2.
        assert_eq!((c.x, c.y), (Some(6), Some(2)));
    }
}
