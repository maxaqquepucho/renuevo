//! Imagen de trabajo para el procesado: un plano `f32` por canal, valores nominales 0..1.
//!
//! El formato planar hace que los filtros recorran memoria contigua por canal (mejor caché y
//! autovectorización) y simplifica trabajar en YCbCr. Una foto en gris lleva un solo plano: se
//! procesa con un tercio de la memoria y del tiempo.

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
    /// Un plano si la foto es en gris; tres en color: R, G, B (o Y, Cb, Cr tras
    /// [`crate::color::rgb_to_ycbcr`]).
    pub channels: Vec<Vec<f32>>,
    pub alpha: Option<Vec<f32>>,
    /// El color está multiplicado por el alfa (ver [`Planar::premultiply`]); `to_image` lo
    /// deshace.
    pub premultiplied: bool,
}

impl Planar {
    pub fn new(width: usize, height: usize, gray: bool, has_alpha: bool) -> Self {
        let n = width * height;
        Self {
            width,
            height,
            channels: vec![vec![0.0; n]; if gray { 1 } else { 3 }],
            alpha: has_alpha.then(|| vec![0.0; n]),
            premultiplied: false,
        }
    }

    pub fn len(&self) -> usize {
        self.width * self.height
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// La foto original era en escala de grises: un solo plano, y se devuelve en gris.
    pub fn is_gray(&self) -> bool {
        self.channels.len() == 1
    }

    /// Los planos R, G y B (en gris, el mismo plano tres veces).
    pub fn rgb(&self) -> [&[f32]; 3] {
        let c = &self.channels;
        let (g, b) = if c.len() == 1 { (0, 0) } else { (1, 2) };
        [&c[0], &c[g], &c[b]]
    }

    /// Multiplica el color por el alfa. Así, al ampliar, el color oculto bajo α = 0 no se mezcla
    /// con el visible (franjas oscuras en los bordes) y el alfa se filtra igual que el color.
    pub fn premultiply(&mut self) {
        let Some(alpha) = &self.alpha else { return };
        if self.premultiplied {
            return;
        }
        for plane in &mut self.channels {
            plane
                .par_iter_mut()
                .zip(alpha.par_iter())
                .for_each(|(c, a)| *c *= a);
        }
        self.premultiplied = true;
    }

    pub fn from_image(image: &DynamicImage) -> Self {
        let (w, h) = (image.width() as usize, image.height() as usize);
        match image {
            DynamicImage::ImageLuma8(b) => from_raw(w, h, b.as_raw(), 1, false, u8_to_f32),
            DynamicImage::ImageLumaA8(b) => from_raw(w, h, b.as_raw(), 2, true, u8_to_f32),
            DynamicImage::ImageRgb8(b) => from_raw(w, h, b.as_raw(), 3, false, u8_to_f32),
            DynamicImage::ImageRgba8(b) => from_raw(w, h, b.as_raw(), 4, true, u8_to_f32),
            DynamicImage::ImageLuma16(b) => from_raw(w, h, b.as_raw(), 1, false, u16_to_f32),
            DynamicImage::ImageLumaA16(b) => from_raw(w, h, b.as_raw(), 2, true, u16_to_f32),
            DynamicImage::ImageRgb16(b) => from_raw(w, h, b.as_raw(), 3, false, u16_to_f32),
            DynamicImage::ImageRgba16(b) => from_raw(w, h, b.as_raw(), 4, true, u16_to_f32),
            DynamicImage::ImageRgb32F(b) => from_raw(w, h, b.as_raw(), 3, false, finite),
            DynamicImage::ImageRgba32F(b) => from_raw(w, h, b.as_raw(), 4, true, finite),
            other => {
                let rgba = other.to_rgba32f();
                from_raw(w, h, rgba.as_raw(), 4, true, finite)
            }
        }
    }

    /// Vuelve a una imagen entera (gris o RGB, con o sin alfa) recortando a 0..1.
    pub fn to_image(&self, depth: BitDepth) -> DynamicImage {
        self.quantize(depth, None)
    }

    /// Como [`Planar::to_image`], pero en 8 bits reparte el error de redondeo del color con un
    /// dither TPDF determinista (de la posición): los degradados suaves que deja el procesado
    /// (un cielo tras quitar el ruido) no forman bandas. Un valor que ya era exacto en 8 bits
    /// no cambia.
    /// `origin` es la posición del píxel (0, 0) en la imagen completa, para que una zona
    /// recortada reciba el mismo dither que en la foto entera.
    pub fn to_image_dithered(&self, depth: BitDepth, origin: (usize, usize)) -> DynamicImage {
        self.quantize(depth, Some(origin))
    }

    fn quantize(&self, depth: BitDepth, dither: Option<(usize, usize)>) -> DynamicImage {
        let (w, h) = (self.width as u32, self.height as u32);
        let channels = self.channels.len() + usize::from(self.alpha.is_some());
        match depth {
            BitDepth::Bits16 => {
                let raw = self.interleave(1.0 / 65535.0, None, |v, _| q16(v));
                match channels {
                    1 => DynamicImage::ImageLuma16(
                        ImageBuffer::<Luma<u16>, _>::from_raw(w, h, raw).expect("tamaño"),
                    ),
                    2 => DynamicImage::ImageLumaA16(
                        ImageBuffer::<LumaA<u16>, _>::from_raw(w, h, raw).expect("tamaño"),
                    ),
                    3 => DynamicImage::ImageRgb16(
                        ImageBuffer::<Rgb<u16>, _>::from_raw(w, h, raw).expect("tamaño"),
                    ),
                    _ => DynamicImage::ImageRgba16(
                        ImageBuffer::<Rgba<u16>, _>::from_raw(w, h, raw).expect("tamaño"),
                    ),
                }
            }
            BitDepth::Bits8 => {
                let raw = self.interleave(1.0 / 255.0, dither, q8);
                match channels {
                    1 => DynamicImage::ImageLuma8(
                        ImageBuffer::<Luma<u8>, _>::from_raw(w, h, raw).expect("tamaño"),
                    ),
                    2 => DynamicImage::ImageLumaA8(
                        ImageBuffer::<LumaA<u8>, _>::from_raw(w, h, raw).expect("tamaño"),
                    ),
                    3 => DynamicImage::ImageRgb8(
                        ImageBuffer::<Rgb<u8>, _>::from_raw(w, h, raw).expect("tamaño"),
                    ),
                    _ => DynamicImage::ImageRgba8(
                        ImageBuffer::<Rgba<u8>, _>::from_raw(w, h, raw).expect("tamaño"),
                    ),
                }
            }
        }
    }

    /// Intercala los planos (y el alfa) píxel a píxel. Si el color está premultiplicado se
    /// divide por el alfa; donde el alfa cuantizado es 0 (por debajo de medio `lsb`) el color
    /// queda en 0. `quantize` recibe el valor y el dither (0 si no hay).
    fn interleave<T: Send + Copy + Default>(
        &self,
        lsb: f32,
        dither: Option<(usize, usize)>,
        quantize: impl Fn(f32, f32) -> T + Sync,
    ) -> Vec<T> {
        let planes = &self.channels;
        let alpha = self.alpha.as_deref();
        let unpremultiply = self.premultiplied && alpha.is_some();
        let channels = planes.len() + usize::from(alpha.is_some());
        let mut out = vec![T::default(); self.len() * channels];
        out.par_chunks_mut(self.width.max(1) * channels)
            .enumerate()
            .for_each(|(y, row)| {
                let start = y * self.width;
                for (x, px) in row.chunks_exact_mut(channels).enumerate() {
                    let i = start + x;
                    let d = |c: usize| dither.map_or(0.0, |(ox, oy)| tpdf(ox + x, oy + y, c));
                    let k = match alpha {
                        Some(a) if unpremultiply => {
                            if a[i] >= 0.5 * lsb {
                                a[i].recip()
                            } else {
                                0.0
                            }
                        }
                        _ => 1.0,
                    };
                    for (c, plane) in planes.iter().enumerate() {
                        px[c] = quantize(plane[i] * k, d(c));
                    }
                    // El alfa sin dither: así «alfa 0 ⇒ color 0» se cumple siempre.
                    if let Some(a) = alpha {
                        px[planes.len()] = quantize(a[i], 0.0);
                    }
                }
            });
        out
    }
}

/// Las filas de varios planos del mismo tamaño, agrupadas por fila (`rows[y][p]` es la fila `y`
/// del plano `p`), para recorrerlas en paralelo sin importar cuántos planos haya.
pub(crate) fn rows_mut<'a>(
    planes: impl IntoIterator<Item = &'a mut [f32]>,
    width: usize,
    height: usize,
) -> Vec<Vec<&'a mut [f32]>> {
    let mut rows: Vec<Vec<&mut [f32]>> = (0..height).map(|_| Vec::with_capacity(4)).collect();
    if width == 0 {
        return rows;
    }
    for plane in planes {
        for (row, chunk) in rows.iter_mut().zip(plane.chunks_mut(width)) {
            row.push(chunk);
        }
    }
    rows
}

const U8: f32 = 1.0 / 255.0;
const U16: f32 = 1.0 / 65535.0;

/// Conversión de un valor de 8 bits a 0..1 (la misma en todo el procesado).
pub(crate) fn u8_to_f32(v: u8) -> f32 {
    f32::from(v) * U8
}

pub(crate) fn u16_to_f32(v: u16) -> f32 {
    f32::from(v) * U16
}

/// Un TIFF en coma flotante puede traer NaN o infinitos: un solo valor así se propagaría por
/// los filtros. NaN y −∞ pasan a 0, +∞ a 1.
pub(crate) fn finite(v: f32) -> f32 {
    if v.is_finite() {
        v
    } else if v == f32::INFINITY {
        1.0
    } else {
        0.0
    }
}

fn q16(v: f32) -> u16 {
    (v.clamp(0.0, 1.0) * 65535.0).round() as u16
}

/// `dither` va en LSB, en (−1, 1). Un valor que ya es exacto en 8 bits (zonas que el procesado
/// no tocó, colores planos) no lo recibe: solo se reparte el error de redondeo donde lo hay.
fn q8(v: f32, dither: f32) -> u8 {
    let x = v.clamp(0.0, 1.0) * 255.0;
    if (x - x.round()).abs() < 1e-3 {
        x.round() as u8
    } else {
        (x + dither).round().clamp(0.0, 255.0) as u8
    }
}

/// Dither TPDF: suma de dos uniformes de ±0,5 LSB, así que va de −1 a 1 LSB con forma
/// triangular. Es el que hace que el error de redondeo no dependa de la señal: con uno de solo
/// ±0,5 LSB en total, en un cielo de prueba quedaban mesetas el doble de largas.
/// Determinista a partir de la posición y el canal: la misma foto da siempre lo mismo.
fn tpdf(x: usize, y: usize, channel: usize) -> f32 {
    // Mezcla de enteros de MurmurHash3 (fmix32).
    let mut h = (x as u32).wrapping_mul(0x9E37_79B1)
        ^ (y as u32).wrapping_mul(0x85EB_CA77)
        ^ (channel as u32).wrapping_mul(0xC2B2_AE3D);
    h ^= h >> 16;
    h = h.wrapping_mul(0x85EB_CA6B);
    h ^= h >> 13;
    h = h.wrapping_mul(0xC2B2_AE35);
    h ^= h >> 16;
    let (u1, u2) = ((h & 0xFFFF) as f32, (h >> 16) as f32);
    // u1 + u2 − 65535 ∈ [−65535, 65535]: el resultado nunca llega a ±1.
    (u1 + u2 - 65535.0) * (1.0 / 65536.0)
}

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
    let color = planar.channels.len();
    let planes = planar
        .channels
        .iter_mut()
        .chain(planar.alpha.as_mut())
        .map(Vec::as_mut_slice);
    rows_mut(planes, width, height)
        .into_par_iter()
        .enumerate()
        .for_each(|(y, mut dst)| {
            let src = &raw[y * width * channels..(y + 1) * width * channels];
            for (x, px) in src.chunks_exact(channels).enumerate() {
                for (p, plane) in dst.iter_mut().enumerate() {
                    // El alfa es siempre el último canal de la fuente.
                    plane[x] = to_f32(px[if p < color { p } else { channels - 1 }]);
                }
            }
        });
    planar
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tpdf_dither_is_triangular_below_one_lsb() {
        let (mut sum, mut max, mut inner) = (0.0f64, 0.0f32, 0);
        for y in 0..256 {
            for x in 0..256 {
                let d = tpdf(x, y, y % 3);
                sum += f64::from(d);
                max = max.max(d.abs());
                inner += usize::from(d.abs() < 0.5);
            }
        }
        assert!(max < 1.0, "{max}");
        assert!((sum / 65536.0).abs() < 0.01, "media {}", sum / 65536.0);
        // Triangular: 3/4 de los valores en (−0,5, 0,5).
        let share = inner as f64 / 65536.0;
        assert!((share - 0.75).abs() < 0.02, "{share}");
    }

    #[test]
    fn dither_keeps_exact_8_bit_values() {
        // Un valor que ya es exacto en 8 bits no cambia con el dither (copia y zonas planas).
        for v in 0..=255u8 {
            for x in 0..64 {
                assert_eq!(q8(u8_to_f32(v), tpdf(x, 7, 1)), v);
            }
        }
    }
}
