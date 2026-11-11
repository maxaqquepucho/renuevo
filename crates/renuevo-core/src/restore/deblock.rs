//! Reducción de bloques JPEG.
//!
//! 1. Detecta la rejilla de 8×8 (puede estar desplazada si la foto se giró o recortó).
//! 2. En cada frontera de bloque, si el salto es pequeño y ambos lados son planos (señal de
//!    artefacto, no de borde real), lo reparte suavemente entre los píxeles vecinos, al estilo
//!    del filtro de bucle de H.264.

use rayon::prelude::*;

use crate::filters::transpose;

const BLOCK: usize = 8;

/// Desfase de la rejilla en x (0..8) si se detecta bloqueo claro.
fn detect_grid_offset(plane: &[f32], w: usize, h: usize) -> Option<usize> {
    if w < 4 * BLOCK || h < 2 {
        return None;
    }
    let step = (h / 256).max(1);
    let mut energy = [0.0f64; BLOCK];
    let mut counts = [0usize; BLOCK];
    for y in (0..h).step_by(step) {
        let row = &plane[y * w..(y + 1) * w];
        for x in 1..w {
            energy[x % BLOCK] += f64::from((row[x] - row[x - 1]).abs());
            counts[x % BLOCK] += 1;
        }
    }
    let means: Vec<f64> = energy
        .iter()
        .zip(counts)
        .map(|(e, c)| e / c.max(1) as f64)
        .collect();
    let (best, &peak) = means.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1))?;
    let others = (means.iter().sum::<f64>() - peak) / (BLOCK - 1) as f64;
    // Con bloqueo real, la frontera tiene claramente más salto que el interior de los bloques.
    (peak > others * 1.08 && peak > 1e-4).then_some(best)
}

/// Suaviza las fronteras verticales (entre x-1 y x, con x ≡ offset mod 8) de cada fila.
fn filter_vertical_edges(plane: &mut [f32], w: usize, offset: usize, strength: f32) {
    let alpha = 0.10 * strength; // salto máximo que consideramos artefacto
    let beta = 0.03 * strength; // planitud exigida a cada lado
    let tc = alpha * 0.5;
    plane.par_chunks_mut(w).for_each(|row| {
        let mut x = if offset == 0 { BLOCK } else { offset };
        while x + 2 < w {
            if x >= 3 {
                let (p2, p1, p0) = (row[x - 3], row[x - 2], row[x - 1]);
                let (q0, q1, q2) = (row[x], row[x + 1], row[x + 2]);
                if (q0 - p0).abs() < alpha && (p1 - p0).abs() < beta && (q1 - q0).abs() < beta {
                    let delta = ((4.0 * (q0 - p0) + (p1 - q1)) / 8.0).clamp(-tc, tc);
                    row[x - 1] = p0 + delta;
                    row[x] = q0 - delta;
                    if (p2 - p0).abs() < beta {
                        row[x - 2] = p1 + delta * 0.5;
                    }
                    if (q2 - q0).abs() < beta {
                        row[x + 1] = q1 - delta * 0.5;
                    }
                }
            }
            x += BLOCK;
        }
    });
}

/// Aplica el deblocking a un plano. Devuelve si detectó (y trató) bloqueo.
pub fn deblock(plane: &mut [f32], w: usize, h: usize, strength: f32) -> bool {
    if strength <= 0.0 {
        return false;
    }
    let mut any = false;
    if let Some(ox) = detect_grid_offset(plane, w, h) {
        filter_vertical_edges(plane, w, ox, strength);
        any = true;
    }
    // Fronteras horizontales: trasponer, filtrar como verticales y deshacer.
    let mut t = transpose(plane, w, h);
    if let Some(oy) = detect_grid_offset(&t, h, w) {
        filter_vertical_edges(&mut t, h, oy, strength);
        plane.copy_from_slice(&transpose(&t, h, w));
        any = true;
    }
    any
}
