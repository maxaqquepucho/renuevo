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

pub fn rgb_to_ycbcr(p: &mut Planar) {
    let [c0, c1, c2] = &mut p.channels;
    c0.par_iter_mut()
        .zip(c1.par_iter_mut())
        .zip(c2.par_iter_mut())
        .for_each(|((r, g), b)| {
            let y = luma(*r, *g, *b);
            let cb = (*b - y) / (2.0 * (1.0 - KB));
            let cr = (*r - y) / (2.0 * (1.0 - KR));
            (*r, *g, *b) = (y, cb, cr);
        });
}

pub fn ycbcr_to_rgb(p: &mut Planar) {
    let [c0, c1, c2] = &mut p.channels;
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
