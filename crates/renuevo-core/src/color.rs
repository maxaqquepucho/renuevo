//! Conversión RGB ↔ YCbCr (BT.601 de rango completo, el mismo espacio que usa JPEG).
//! Cb y Cr quedan centrados en 0.

use rayon::prelude::*;

use crate::planar::Planar;

const KR: f32 = 0.299;
const KG: f32 = 0.587;
const KB: f32 = 0.114;

#[inline]
pub fn luma(r: f32, g: f32, b: f32) -> f32 {
    KR * r + KG * g + KB * b
}

/// Y, Cb y Cr de un píxel (la misma cuenta en todo el procesado, también al medir el ruido).
#[inline]
pub(crate) fn to_ycbcr(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    let y = luma(r, g, b);
    (
        y,
        (b - y) / (2.0 * (1.0 - KB)),
        (r - y) / (2.0 * (1.0 - KR)),
    )
}

/// En gris no hace nada: el único plano ya es la luminancia.
pub fn rgb_to_ycbcr(p: &mut Planar) {
    let [c0, c1, c2] = &mut p.channels[..] else {
        return;
    };
    c0.par_iter_mut()
        .zip(c1.par_iter_mut())
        .zip(c2.par_iter_mut())
        .for_each(|((r, g), b)| (*r, *g, *b) = to_ycbcr(*r, *g, *b));
}

pub fn ycbcr_to_rgb(p: &mut Planar) {
    let [c0, c1, c2] = &mut p.channels[..] else {
        return;
    };
    c0.par_iter_mut()
        .zip(c1.par_iter_mut())
        .zip(c2.par_iter_mut())
        .for_each(|((y, cb), cr)| {
            let r = *y + 2.0 * (1.0 - KR) * *cr;
            let b = *y + 2.0 * (1.0 - KB) * *cb;
            let g = (*y - KR * r - KB * b) / KG;
            (*y, *cb, *cr) = (r, g, b);
        });
}
