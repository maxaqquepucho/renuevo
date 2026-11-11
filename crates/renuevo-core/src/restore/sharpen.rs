//! Máscara de enfoque (unsharp mask) con umbral de ruido y control de halos.
//!
//! - **Umbral («coring»)**: el detalle por debajo de `threshold` se considera ruido y no se
//!   amplifica; así enfocar no devuelve el grano que se acaba de quitar.
//! - **Halos**: el resultado se limita al rango [mín, máx] de la vecindad 3×3 original más un
//!   pequeño margen, sin los contornos claros/oscuros típicos del enfoque fuerte.

use rayon::prelude::*;

use crate::filters::{gaussian_blur, min_max_3x3};

/// `amount` en 0..1 (ganancia 0..1,5), `radius` = σ del desenfoque en píxeles,
/// `threshold` = amplitud de detalle (escala 0..1) que no se toca.
pub fn unsharp(plane: &mut [f32], w: usize, h: usize, amount: f32, radius: f32, threshold: f32) {
    if amount <= 0.0 {
        return;
    }
    let gain = amount * 1.5;
    let blurred = gaussian_blur(plane, w, h, radius);
    let (lo, hi) = min_max_3x3(plane, w, h);
    const OVERSHOOT: f32 = 0.01;
    plane.par_iter_mut().enumerate().for_each(|(k, v)| {
        let detail = *v - blurred[k];
        let cored = detail.signum() * (detail.abs() - threshold).max(0.0);
        let sharpened = *v + gain * cored;
        // max/min en lugar de clamp: clamp entra en pánico si el rango no es válido.
        *v = sharpened.max(lo[k] - OVERSHOOT).min(hi[k] + OVERSHOOT);
    });
}
