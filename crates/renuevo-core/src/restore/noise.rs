//! Estimación automática del nivel de ruido.
//!
//! Se mide la desviación típica de cada bloque 8×8 de la luminancia y se toma el percentil 10:
//! en las zonas planas de una foto (cielo, paredes) toda la variación es ruido. A diferencia de
//! los estimadores de alta frecuencia, esto también capta el ruido «manchado» que deja la
//! compresión JPEG/HEIC, y no confunde la textura fina de una foto limpia con ruido.
//!
//! No cuentan los bloques que no dicen nada del ruido:
//! - los **recortados**, con al menos un cuarto de sus píxeles en negro o blanco puros o no del
//!   todo opacos (marcos, cielos quemados, zonas transparentes): ahí el ruido se pierde contra
//!   0 o 1, y con un 15 % de marco negro el percentil 10 caía en el marco y daba σ = 0;
//! - en la luminancia, los **planos** (σ < 0,5/255, por debajo de la cuantización de 8 bits):
//!   son zonas sintéticas o que perdieron el ruido al comprimir. En el color no se descartan:
//!   JPEG cuantiza mucho el color y un bloque de color plano es normal en una foto limpia.
//!
//! Si entre los dos son más de la mitad de la foto (un gráfico, un logotipo, un JPEG destrozado),
//! lo que queda son sobre todo bordes, no ruido: entonces se usan todos los bloques, como antes,
//! y una foto sin ruido sigue dando ≈ 0 en lugar de inventarlo.
//!
//! La foto se recorre una sola vez, en tiras de 16 filas en paralelo y convirtiendo cada píxel
//! una vez, a través de [`YccRows`]: desde el búfer de 8/16 bits recién abierto (sin copiarla
//! entera a `f32`) o desde un [`Planar`], con exactamente el mismo resultado. En la misma pasada
//! se mide la rejilla de bloques JPEG (ver [`super::deblock`]).

use rayon::prelude::*;

use super::NoiseLevels;
use super::deblock::{BlockGrid, PlaneGrid, pick_offset};
use crate::color::to_ycbcr;
use crate::planar::Planar;

const BLOCK: usize = 8;
/// JPEG/HEIC submuestrean el color: sus manchas abarcan bloques de 16×16.
const CHROMA_BLOCK: usize = 16;
const PERCENTILE: f32 = 0.10;
/// z del percentil 10 de una normal.
const Z_P10: f32 = 1.2816;
/// Un bloque con menos variación que esto (escala 0..1) no tiene ruido medible.
const FLAT_SIGMA: f32 = 0.5 / 255.0;
/// Margen para considerar un píxel negro o blanco puro.
const CLIP_EPS: f32 = 0.5 / 255.0;

/// Una foto vista por filas en YCbCr (escala 0..1, el color premultiplicado por el alfa si lo
/// hay, como lo ve la restauración).
pub(crate) trait YccRows: Sync {
    fn size(&self) -> (usize, usize);
    /// 1 en gris (solo Y), 3 en color (Y, Cb, Cr).
    fn planes(&self) -> usize;
    /// Copia la fila `y` de cada plano en `out[p]` y la del alfa en `alpha` (1 si no hay).
    fn row(&self, y: usize, out: &mut [&mut [f32]], alpha: &mut [f32]);
}

/// Un búfer intercalado de 1 a 4 canales (gris, gris+alfa, RGB, RGBA).
pub(crate) struct Interleaved<'a, T> {
    pub raw: &'a [T],
    pub width: usize,
    pub height: usize,
    pub channels: usize,
    pub has_alpha: bool,
    pub to_f32: fn(T) -> f32,
}

impl<T: Copy + Sync> YccRows for Interleaved<'_, T> {
    fn size(&self) -> (usize, usize) {
        (self.width, self.height)
    }

    fn planes(&self) -> usize {
        if self.channels <= 2 { 1 } else { 3 }
    }

    fn row(&self, y: usize, out: &mut [&mut [f32]], alpha: &mut [f32]) {
        let (f, c) = (self.to_f32, self.channels);
        let n = self.width * c;
        for (x, px) in self.raw[y * n..(y + 1) * n].chunks_exact(c).enumerate() {
            let a = if self.has_alpha { f(px[c - 1]) } else { 1.0 };
            alpha[x] = a;
            // Mismas operaciones y en el mismo orden que `Planar::from_image` + `premultiply` +
            // `rgb_to_ycbcr`: el resultado es idéntico bit a bit.
            let premultiply = |v: f32| if self.has_alpha { v * a } else { v };
            if c <= 2 {
                out[0][x] = premultiply(f(px[0]));
            } else {
                let (yy, cb, cr) = to_ycbcr(
                    premultiply(f(px[0])),
                    premultiply(f(px[1])),
                    premultiply(f(px[2])),
                );
                (out[0][x], out[1][x], out[2][x]) = (yy, cb, cr);
            }
        }
    }
}

/// Un [`Planar`] ya convertido a YCbCr (y premultiplicado si tiene alfa).
pub(crate) struct PlanarRows<'a>(pub &'a Planar);

impl YccRows for PlanarRows<'_> {
    fn size(&self) -> (usize, usize) {
        (self.0.width, self.0.height)
    }

    fn planes(&self) -> usize {
        self.0.channels.len()
    }

    fn row(&self, y: usize, out: &mut [&mut [f32]], alpha: &mut [f32]) {
        let span = y * self.0.width..(y + 1) * self.0.width;
        for (o, plane) in out.iter_mut().zip(&self.0.channels) {
            o.copy_from_slice(&plane[span.clone()]);
        }
        match &self.0.alpha {
            Some(a) => alpha.copy_from_slice(&a[span]),
            None => alpha.fill(1.0),
        }
    }
}

/// Desviación típica estimada del ruido, en la escala 0..1 del plano (de luminancia).
pub fn estimate_sigma(plane: &[f32], w: usize, h: usize) -> f32 {
    estimate_sigma_blocks(plane, w, h, BLOCK)
}

/// Igual que [`estimate_sigma`] con bloques de `block`×`block` píxeles.
pub fn estimate_sigma_blocks(plane: &[f32], w: usize, h: usize, block: usize) -> f32 {
    if w < block || h < block {
        return 0.0;
    }
    let stats: Vec<(f32, bool)> = (0..h / block)
        .into_par_iter()
        .flat_map_iter(|by| {
            let rows = &plane[by * block * w..(by + 1) * block * w];
            let clipped: Vec<bool> = rows.iter().map(|&l| is_clipped(l, 1.0)).collect();
            block_row_stats(rows, clipped, w, block)
        })
        .collect();
    select(stats, block, true)
}

/// Lo que mide cada tira de filas.
struct Strip {
    /// (σ, recortado) de los bloques 8×8 de luminancia y 16×16 de Cb y Cr.
    luma: Vec<(f32, bool)>,
    chroma: [Vec<(f32, bool)>; 2],
    /// Saltos entre columnas vecinas (filas muestreadas) y entre filas vecinas (columnas
    /// muestreadas), por plano y por resto módulo 8.
    grid_x: [[f64; BLOCK]; 3],
    grid_y: [[f64; BLOCK]; 3],
}

/// Ruido de luminancia y de color, y rejilla de bloques JPEG de `src`, en una sola pasada.
pub(crate) fn measure<S: YccRows>(src: &S) -> NoiseLevels {
    const STRIP: usize = CHROMA_BLOCK;
    let (w, h) = src.size();
    let planes = src.planes();
    if w == 0 || h == 0 {
        return NoiseLevels {
            grid: Some(BlockGrid::default()),
            ..NoiseLevels::default()
        };
    }
    // La rejilla se mide en ~256 filas y ~256 columnas repartidas por la foto.
    let row_step = (h / 256).max(1);
    let cols: Vec<usize> = (0..w).step_by((w / 256).max(1)).collect();

    let strips: Vec<Strip> = (0..h.div_ceil(STRIP))
        .into_par_iter()
        .map(|s| {
            let y0 = s * STRIP;
            let rows = STRIP.min(h - y0);
            // Fila 0: la anterior a la tira (para los saltos verticales de su frontera).
            let mut bufs = vec![vec![0.0f32; (rows + 1) * w]; planes];
            let mut alpha = vec![0.0f32; (rows + 1) * w];
            for r in usize::from(y0 == 0)..=rows {
                let mut out: Vec<&mut [f32]> = bufs
                    .iter_mut()
                    .map(|b| &mut b[r * w..(r + 1) * w])
                    .collect();
                src.row(y0 + r - 1, &mut out, &mut alpha[r * w..(r + 1) * w]);
            }
            let row = |p: usize, r: usize| &bufs[p][r * w..(r + 1) * w];
            // Píxeles recortados de la tira (sin la fila anterior), según la luminancia y el alfa.
            let clipped: Vec<bool> = bufs[0][w..]
                .iter()
                .zip(&alpha[w..])
                .map(|(&l, &a)| is_clipped(l, a))
                .collect();
            // Filas `from..from + block` de la tira.
            let span = |from: usize, block: usize| (from + 1) * w..(from + 1 + block) * w;
            let mask = |from: usize, block: usize| &clipped[from * w..(from + block) * w];

            let mut luma = Vec::new();
            for k in 0..STRIP / BLOCK {
                if (y0 + (k + 1) * BLOCK) <= h / BLOCK * BLOCK {
                    let y = &bufs[0][span(k * BLOCK, BLOCK)];
                    luma.extend(block_row_stats(y, mask(k * BLOCK, BLOCK), w, BLOCK));
                }
            }
            let mut chroma = [Vec::new(), Vec::new()];
            if planes == 3 && rows == CHROMA_BLOCK {
                for (c, stats) in chroma.iter_mut().enumerate() {
                    let values = &bufs[c + 1][span(0, CHROMA_BLOCK)];
                    let m = mask(0, CHROMA_BLOCK);
                    stats.extend(block_row_stats(values, m, w, CHROMA_BLOCK));
                }
            }

            let (mut grid_x, mut grid_y) = ([[0.0f64; BLOCK]; 3], [[0.0f64; BLOCK]; 3]);
            for r in 1..=rows {
                let y = y0 + r - 1;
                for p in 0..planes {
                    let cur = row(p, r);
                    if y.is_multiple_of(row_step) {
                        for x in 1..w {
                            grid_x[p][x % BLOCK] += f64::from((cur[x] - cur[x - 1]).abs());
                        }
                    }
                    if y > 0 {
                        let prev = row(p, r - 1);
                        for &x in &cols {
                            grid_y[p][y % BLOCK] += f64::from((cur[x] - prev[x]).abs());
                        }
                    }
                }
            }
            Strip {
                luma,
                chroma,
                grid_x,
                grid_y,
            }
        })
        .collect();

    // Todo se junta en el orden de las tiras: el resultado no depende del reparto del trabajo.
    let mut luma = Vec::new();
    let mut chroma = [Vec::new(), Vec::new()];
    let (mut grid_x, mut grid_y) = ([[0.0f64; BLOCK]; 3], [[0.0f64; BLOCK]; 3]);
    for strip in strips {
        luma.extend(strip.luma);
        for (all, part) in chroma.iter_mut().zip(strip.chroma) {
            all.extend(part);
        }
        for p in 0..planes {
            for k in 0..BLOCK {
                grid_x[p][k] += strip.grid_x[p][k];
                grid_y[p][k] += strip.grid_y[p][k];
            }
        }
    }
    let mut grid = BlockGrid::default();
    let sampled_rows = h.div_ceil(row_step);
    for (p, g) in grid.planes.iter_mut().enumerate().take(planes) {
        *g = PlaneGrid {
            x: (w >= 4 * BLOCK && h >= 2)
                .then(|| pick_offset(&grid_x[p], w, sampled_rows))
                .flatten(),
            y: (h >= 4 * BLOCK && w >= 2)
                .then(|| pick_offset(&grid_y[p], h, cols.len()))
                .flatten(),
        };
    }
    let [cb, cr] = chroma;
    NoiseLevels {
        luma: select(luma, BLOCK, true),
        chroma: if planes == 3 {
            select(cb, CHROMA_BLOCK, false).max(select(cr, CHROMA_BLOCK, false))
        } else {
            0.0
        },
        grid: Some(grid),
    }
}

/// Un píxel negro o blanco puro, o no del todo opaco: su ruido no se puede medir.
#[inline]
fn is_clipped(luma: f32, alpha: f32) -> bool {
    alpha < 1.0 || luma <= CLIP_EPS || luma >= 1.0 - CLIP_EPS
}

/// (σ, recortado) de cada bloque `block`×`block` de una franja de `block` filas; `clipped`
/// marca los píxeles recortados de la franja.
fn block_row_stats<'a, M: AsRef<[bool]> + 'a>(
    values: &'a [f32],
    clipped: M,
    w: usize,
    block: usize,
) -> impl Iterator<Item = (f32, bool)> + 'a {
    let n = block * block;
    (0..w / block).map(move |bx| {
        let (mut sum, mut sum2) = (0.0f64, 0.0f64);
        let mut clipped_px = 0;
        for r in 0..block {
            let span = r * w + bx * block..r * w + (bx + 1) * block;
            for &v in &values[span.clone()] {
                let v = f64::from(v);
                sum += v;
                sum2 += v * v;
            }
            clipped_px += clipped.as_ref()[span].iter().filter(|&&c| c).count();
        }
        let mean = sum / n as f64;
        let std = (sum2 / n as f64 - mean * mean).max(0.0).sqrt() as f32;
        (std, 4 * clipped_px >= n)
    })
}

/// Percentil 10 de la σ de los bloques útiles (ver la explicación del módulo), corregido para
/// estimar la σ del ruido.
fn select(blocks: Vec<(f32, bool)>, block: usize, skip_flat: bool) -> f32 {
    let useful: Vec<f32> = blocks
        .iter()
        .filter(|&&(s, clipped)| !clipped && (!skip_flat || s >= FLAT_SIGMA))
        .map(|&(s, _)| s)
        .collect();
    let mut stds = if 2 * useful.len() >= blocks.len() {
        useful
    } else {
        blocks.into_iter().map(|(s, _)| s).collect()
    };
    if stds.is_empty() {
        return 0.0;
    }
    let k = ((stds.len() as f32 * PERCENTILE) as usize).min(stds.len() - 1);
    let (_, p, _) = stds.select_nth_unstable_by(k, f32::total_cmp);
    // Con ruido blanco, la desviación de n muestras tiene error típico ≈ σ/√(2(n-1)):
    // su percentil 10 es ≈ σ·(1 − z·/√(2(n-1))). Lo deshacemos para obtener σ.
    let n = (block * block) as f32;
    *p / (1.0 - Z_P10 / (2.0 * (n - 1.0)).sqrt())
}
