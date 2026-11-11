//! Imagen de trabajo para el procesado: un plano `f32` por canal, valores nominales 0..1.
//!
//! El formato planar hace que los filtros recorran memoria contigua por canal (mejor caché y
//! autovectorización) y simplifica trabajar en YCbCr.

use image::{DynamicImage, ImageBuffer, Luma, LumaA, Rgb, Rgba};
use rayon::prelude::*;

/// Profundidad de bits de la imagen que se genera tras procesar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BitDepth {
    /// Por defecto: evita bandas al volver de `f32`.
    #[default]
    Bits16,
    Bits8,
}

#[derive(Debug, Clone)]
pub struct Planar {
    pub width: usize,
    pub height: usize,
    /// R, G, B (o Y, Cb, Cr tras [`crate::color::rgb_to_ycbcr`]).
    pub channels: [Vec<f32>; 3],
    pub alpha: Option<Vec<f32>>,
    /// La foto original era en escala de grises: se devuelve en gris.
    pub gray: bool,
}

impl Planar {
    pub fn new(width: usize, height: usize, gray: bool, has_alpha: bool) -> Self {
        let n = width * height;
        Self {
            width,
            height,
            channels: [vec![0.0; n], vec![0.0; n], vec![0.0; n]],
            alpha: has_alpha.then(|| vec![0.0; n]),
            gray,
        }
    }

    pub fn len(&self) -> usize {
        self.width * self.height
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn from_image(image: &DynamicImage) -> Self {
        const U8: f32 = 1.0 / 255.0;
        const U16: f32 = 1.0 / 65535.0;
        let (w, h) = (image.width() as usize, image.height() as usize);
        match image {
            DynamicImage::ImageLuma8(b) => {
                from_raw(w, h, b.as_raw(), 1, false, |v| f32::from(v) * U8)
            }
            DynamicImage::ImageLumaA8(b) => {
                from_raw(w, h, b.as_raw(), 2, true, |v| f32::from(v) * U8)
            }
            DynamicImage::ImageRgb8(b) => {
                from_raw(w, h, b.as_raw(), 3, false, |v| f32::from(v) * U8)
            }
            DynamicImage::ImageRgba8(b) => {
                from_raw(w, h, b.as_raw(), 4, true, |v| f32::from(v) * U8)
            }
            DynamicImage::ImageLuma16(b) => {
                from_raw(w, h, b.as_raw(), 1, false, |v| f32::from(v) * U16)
            }
            DynamicImage::ImageLumaA16(b) => {
                from_raw(w, h, b.as_raw(), 2, true, |v| f32::from(v) * U16)
            }
            DynamicImage::ImageRgb16(b) => {
                from_raw(w, h, b.as_raw(), 3, false, |v| f32::from(v) * U16)
            }
            DynamicImage::ImageRgba16(b) => {
                from_raw(w, h, b.as_raw(), 4, true, |v| f32::from(v) * U16)
            }
            DynamicImage::ImageRgb32F(b) => from_raw(w, h, b.as_raw(), 3, false, finite),
            DynamicImage::ImageRgba32F(b) => from_raw(w, h, b.as_raw(), 4, true, finite),
            other => {
                let rgba = other.to_rgba32f();
                from_raw(w, h, rgba.as_raw(), 4, true, finite)
            }
        }
    }

    /// Vuelve a una imagen entera (RGB o gris, con o sin alfa) recortando a 0..1.
    pub fn to_image(&self, depth: BitDepth) -> DynamicImage {
        let (w, h) = (self.width as u32, self.height as u32);
        let alpha = self.alpha.is_some();
        match (depth, self.gray, alpha) {
            (BitDepth::Bits16, false, false) => DynamicImage::ImageRgb16(
                ImageBuffer::<Rgb<u16>, _>::from_raw(w, h, self.interleave(3, q16))
                    .expect("tamaño"),
            ),
            (BitDepth::Bits16, false, true) => DynamicImage::ImageRgba16(
                ImageBuffer::<Rgba<u16>, _>::from_raw(w, h, self.interleave(4, q16))
                    .expect("tamaño"),
            ),
            (BitDepth::Bits16, true, false) => DynamicImage::ImageLuma16(
                ImageBuffer::<Luma<u16>, _>::from_raw(w, h, self.interleave(1, q16))
                    .expect("tamaño"),
            ),
            (BitDepth::Bits16, true, true) => DynamicImage::ImageLumaA16(
                ImageBuffer::<LumaA<u16>, _>::from_raw(w, h, self.interleave(2, q16))
                    .expect("tamaño"),
            ),
            (BitDepth::Bits8, false, false) => DynamicImage::ImageRgb8(
                ImageBuffer::<Rgb<u8>, _>::from_raw(w, h, self.interleave(3, q8)).expect("tamaño"),
            ),
            (BitDepth::Bits8, false, true) => DynamicImage::ImageRgba8(
                ImageBuffer::<Rgba<u8>, _>::from_raw(w, h, self.interleave(4, q8)).expect("tamaño"),
            ),
            (BitDepth::Bits8, true, false) => DynamicImage::ImageLuma8(
                ImageBuffer::<Luma<u8>, _>::from_raw(w, h, self.interleave(1, q8)).expect("tamaño"),
            ),
            (BitDepth::Bits8, true, true) => DynamicImage::ImageLumaA8(
                ImageBuffer::<LumaA<u8>, _>::from_raw(w, h, self.interleave(2, q8))
                    .expect("tamaño"),
            ),
        }
    }

    /// Intercala los planos en `channels` canales (1 = gris, 2 = gris+alfa, 3 = RGB, 4 = RGBA).
    fn interleave<T: Send + Copy + Default>(
        &self,
        channels: usize,
        quantize: fn(f32) -> T,
    ) -> Vec<T> {
        let [r, g, b] = &self.channels;
        let alpha = self.alpha.as_deref();
        let mut out = vec![T::default(); self.len() * channels];
        out.par_chunks_mut(self.width.max(1) * channels)
            .enumerate()
            .for_each(|(y, row)| {
                let start = y * self.width;
                for (x, px) in row.chunks_exact_mut(channels).enumerate() {
                    let i = start + x;
                    match channels {
                        1 => px[0] = quantize(gray_of(r[i], g[i], b[i])),
                        2 => {
                            px[0] = quantize(gray_of(r[i], g[i], b[i]));
                            px[1] = quantize(alpha.map_or(1.0, |a| a[i]));
                        }
                        3 => {
                            px[0] = quantize(r[i]);
                            px[1] = quantize(g[i]);
                            px[2] = quantize(b[i]);
                        }
                        _ => {
                            px[0] = quantize(r[i]);
                            px[1] = quantize(g[i]);
                            px[2] = quantize(b[i]);
                            px[3] = quantize(alpha.map_or(1.0, |a| a[i]));
                        }
                    }
                }
            });
        out
    }
}

/// Un TIFF en coma flotante puede traer NaN o infinitos: un solo valor así se propagaría por
/// los filtros. NaN y −∞ pasan a 0, +∞ a 1.
fn finite(v: f32) -> f32 {
    if v.is_finite() {
        v
    } else if v == f32::INFINITY {
        1.0
    } else {
        0.0
    }
}

/// En imágenes grises los tres planos son iguales; tras procesar pueden diferir mínimamente.
fn gray_of(r: f32, g: f32, b: f32) -> f32 {
    (r + g + b) * (1.0 / 3.0)
}

fn q16(v: f32) -> u16 {
    (v.clamp(0.0, 1.0) * 65535.0).round() as u16
}

fn q8(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// Filas paralelas de los tres planos (R, G, B).
type RowTriple<'a> = ((&'a mut [f32], &'a mut [f32]), &'a mut [f32]);

fn from_raw<T: Copy + Sync>(
    width: usize,
    height: usize,
    raw: &[T],
    channels: usize,
    has_alpha: bool,
    to_f32: impl Fn(T) -> f32 + Sync,
) -> Planar {
    let gray = channels <= 2;
    let mut planar = Planar::new(width, height, gray, has_alpha);
    let [r, g, b] = &mut planar.channels;
    let row = width.max(1);
    let fill = |(y, ((r, g), b)): (usize, RowTriple<'_>)| {
        let src = &raw[y * width * channels..(y + 1) * width * channels];
        for (x, px) in src.chunks_exact(channels).enumerate() {
            let (vr, vg, vb) = if gray {
                let v = to_f32(px[0]);
                (v, v, v)
            } else {
                (to_f32(px[0]), to_f32(px[1]), to_f32(px[2]))
            };
            r[x] = vr;
            g[x] = vg;
            b[x] = vb;
        }
    };
    r.par_chunks_mut(row)
        .zip(g.par_chunks_mut(row))
        .zip(b.par_chunks_mut(row))
        .enumerate()
        .for_each(fill);
    if let Some(a) = planar.alpha.as_mut() {
        let alpha_index = channels - 1;
        a.par_chunks_mut(row).enumerate().for_each(|(y, dst)| {
            let src = &raw[y * width * channels..(y + 1) * width * channels];
            for (x, px) in src.chunks_exact(channels).enumerate() {
                dst[x] = to_f32(px[alpha_index]);
            }
        });
    }
    planar
}
