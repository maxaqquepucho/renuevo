//! Estimación automática del nivel de ruido.
//!
//! Se mide la desviación típica de cada bloque 8×8 de la luminancia y se toma el percentil 10:
//! en las zonas planas de una foto (cielo, paredes) toda la variación es ruido. A diferencia de
//! los estimadores de alta frecuencia, esto también capta el ruido «manchado» que deja la
//! compresión JPEG/HEIC, y no confunde la textura fina de una foto limpia con ruido.

use rayon::prelude::*;

const BLOCK: usize = 8;
const PERCENTILE: f32 = 0.10;
/// z del percentil 10 de una normal.
const Z_P10: f32 = 1.2816;

/// Desviación típica estimada del ruido, en la escala 0..1 del plano.
pub fn estimate_sigma(plane: &[f32], w: usize, h: usize) -> f32 {
    estimate_sigma_blocks(plane, w, h, BLOCK)
}

/// Igual que [`estimate_sigma`] con bloques de `block`×`block` píxeles.
pub fn estimate_sigma_blocks(plane: &[f32], w: usize, h: usize, block: usize) -> f32 {
    if w < block || h < block {
        return 0.0;
    }
    let mut stds: Vec<f32> = (0..h / block)
        .into_par_iter()
        .flat_map_iter(|by| {
            (0..w / block).map(move |bx| {
                let (mut sum, mut sum2) = (0.0f64, 0.0f64);
                for y in by * block..(by + 1) * block {
                    for &v in &plane[y * w + bx * block..y * w + (bx + 1) * block] {
                        let v = f64::from(v);
                        sum += v;
                        sum2 += v * v;
                    }
                }
                let n = (block * block) as f64;
                let mean = sum / n;
                (sum2 / n - mean * mean).max(0.0).sqrt() as f32
            })
        })
        .collect();
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
