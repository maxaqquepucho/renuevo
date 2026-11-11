#![allow(dead_code)]
//! Imágenes sintéticas para los tests (sin depender de archivos externos).

use image::{DynamicImage, ImageBuffer, Luma, Rgb, Rgba};

/// RGB 8 bits con degradados y textura: cubre todo el rango de valores.
pub fn rgb8(w: u32, h: u32) -> DynamicImage {
    DynamicImage::ImageRgb8(ImageBuffer::from_fn(w, h, |x, y| {
        Rgb([
            (x * 255 / w.max(1)) as u8,
            (y * 255 / h.max(1)) as u8,
            ((x * 7 + y * 13) % 256) as u8,
        ])
    }))
}

/// RGB 16 bits donde el byte bajo también varía (un PNG de 8 bits no podría representarlo).
pub fn rgb16(w: u32, h: u32) -> DynamicImage {
    DynamicImage::ImageRgb16(ImageBuffer::from_fn(w, h, |x, y| {
        Rgb([
            ((x * 65535 / w.max(1)) ^ (y * 31)) as u16,
            ((y * 65535 / h.max(1)) ^ (x * 17)) as u16,
            ((x * 977 + y * 1543) % 65536) as u16,
        ])
    }))
}

pub fn rgba8(w: u32, h: u32) -> DynamicImage {
    DynamicImage::ImageRgba8(ImageBuffer::from_fn(w, h, |x, y| {
        Rgba([
            (x * 3) as u8,
            (y * 5) as u8,
            ((x ^ y) * 9) as u8,
            ((x + y) * 4 % 256) as u8,
        ])
    }))
}

pub fn luma16(w: u32, h: u32) -> DynamicImage {
    DynamicImage::ImageLuma16(ImageBuffer::from_fn(w, h, |x, y| {
        Luma([((x * 1031 + y * 2063) % 65536) as u16])
    }))
}

/// Foto "plana" en cuadrantes de colores, útil para comprobar orientación tras JPEG.
pub fn quadrants(w: u32, h: u32) -> DynamicImage {
    DynamicImage::ImageRgb8(ImageBuffer::from_fn(w, h, |x, y| {
        match (x < w / 2, y < h / 2) {
            (true, true) => Rgb([230, 20, 20]),    // arriba-izquierda: rojo
            (false, true) => Rgb([20, 200, 20]),   // arriba-derecha: verde
            (true, false) => Rgb([20, 20, 230]),   // abajo-izquierda: azul
            (false, false) => Rgb([230, 230, 30]), // abajo-derecha: amarillo
        }
    }))
}

/// Error absoluto medio por canal en escala 0..255 (para comparar formatos con pérdida).
pub fn mean_abs_diff_8(a: &DynamicImage, b: &DynamicImage) -> f64 {
    let (a, b) = (a.to_rgb8(), b.to_rgb8());
    assert_eq!(a.dimensions(), b.dimensions());
    let sum: u64 = a
        .as_raw()
        .iter()
        .zip(b.as_raw())
        .map(|(x, y)| u64::from(x.abs_diff(*y)))
        .sum();
    sum as f64 / a.as_raw().len() as f64
}
