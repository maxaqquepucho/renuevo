//! Preparar píxeles para la pantalla: miniaturas, recortes a resolución completa y conversión
//! del perfil de color de la foto a sRGB. **Solo afecta a lo que se ve**, nunca a lo exportado.

use std::sync::Arc;

use egui::ColorImage;
use fast_image_resize::images::{Image as FirImage, ImageRef};
use fast_image_resize::{FilterType, IntoImageView, PixelType, ResizeAlg, ResizeOptions, Resizer};
use image::{DynamicImage, ImageBuffer, Luma, LumaA, Rgb, Rgba};
use moxcms::{
    CicpColorPrimaries, ColorProfile, DataColorSpace, Layout, RenderingIntent,
    TransferCharacteristics, Transform16BitExecutor, TransformOptions,
};
use rayon::prelude::*;
use renuevo_core::Photo;

/// Imagen compartida sin copiarla: la foto original o un resultado procesado.
pub enum SharedImage {
    Photo(Arc<Photo>),
    Image(Arc<DynamicImage>),
}

impl SharedImage {
    fn get(&self) -> &DynamicImage {
        match self {
            Self::Photo(p) => &p.image,
            Self::Image(i) => i,
        }
    }
}

pub struct DisplaySource {
    image: SharedImage,
    to_srgb: ToSrgb,
}

/// Cómo se pasan los colores de la foto a sRGB para la pantalla.
enum ToSrgb {
    /// Sin perfil (ya es sRGB) o con uno que no se puede usar.
    None,
    /// Perfil RGB, en 16 bits: cuantizar a 8 antes de convertir crearía bandas.
    Rgb(Arc<Transform16BitExecutor>),
    /// Perfil gris: tabla con los 65 536 niveles ya en sRGB.
    Gray(Arc<[[u8; 3]]>),
    /// HDR (PQ o HLG): mapeo de tonos parecido al de macOS en una pantalla normal.
    Hdr(Hdr),
}

impl ToSrgb {
    fn for_profile(src: &ColorProfile) -> Self {
        if let Some(hdr) = Hdr::from_profile(src) {
            return Self::Hdr(hdr);
        }
        let srgb = ColorProfile::new_srgb();
        // La intención de la cabecera del perfil, como ColorSync; si el perfil no trae tabla
        // para ella, la perceptual.
        let intents = [src.rendering_intent, RenderingIntent::Perceptual];
        let options = |rendering_intent| TransformOptions {
            rendering_intent,
            ..TransformOptions::default()
        };
        match src.color_space {
            DataColorSpace::Rgb => intents
                .into_iter()
                .find_map(|i| {
                    src.create_transform_16bit(Layout::Rgba, &srgb, Layout::Rgba, options(i))
                        .ok()
                })
                .map_or(Self::None, Self::Rgb),
            DataColorSpace::Gray => intents
                .into_iter()
                .find_map(|i| gray_table(src, &srgb, options(i)))
                .map_or(Self::None, Self::Gray),
            _ => Self::None,
        }
    }
}

fn gray_table(
    src: &ColorProfile,
    srgb: &ColorProfile,
    options: TransformOptions,
) -> Option<Arc<[[u8; 3]]>> {
    let transform = src
        .create_transform_16bit(Layout::Gray, srgb, Layout::Rgb, options)
        .ok()?;
    let input: Vec<u16> = (0..=u16::MAX).collect();
    let mut out = vec![0u16; input.len() * 3];
    transform.transform(&input, &mut out).ok()?;
    Some(
        out.as_chunks::<3>()
            .0
            .iter()
            .map(|c| c.map(to_u8))
            .collect(),
    )
}

fn to_u8(v: u16) -> u8 {
    ((u32::from(v) * 255 + 32_767) / 65_535) as u8
}

/// Aproximación a cómo muestra macOS una foto HDR en una pantalla SDR: el blanco de referencia
/// (203 cd/m²) queda a la mitad de la luz máxima y las luces altas se comprimen suavemente.
#[derive(Clone, Copy)]
struct Hdr {
    hlg: bool,
    /// De las primarias de la foto a las de sRGB, en luz lineal.
    to_srgb: [[f32; 3]; 3],
}

impl Hdr {
    fn from_profile(profile: &ColorProfile) -> Option<Self> {
        let cicp = profile.cicp.as_ref()?;
        let hlg = match cicp.transfer_characteristics {
            TransferCharacteristics::Smpte2084 => false,
            TransferCharacteristics::Hlg => true,
            _ => return None,
        };
        let to_srgb = match cicp.color_primaries {
            CicpColorPrimaries::Bt2020 => [
                [1.6605, -0.5876, -0.0728],
                [-0.1246, 1.1329, -0.0083],
                [-0.0182, -0.1006, 1.1187],
            ],
            CicpColorPrimaries::Smpte432 => [
                [1.2249, -0.2247, 0.0],
                [-0.0420, 1.0419, 0.0],
                [-0.0197, -0.0786, 1.0979],
            ],
            CicpColorPrimaries::Bt709 => [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
            _ => return None,
        };
        Some(Self { hlg, to_srgb })
    }

    /// Píxel codificado (0..1) → sRGB de 8 bits.
    fn map(&self, [r, g, b, a]: [f32; 4]) -> [u8; 4] {
        const REFERENCE_WHITE: f32 = 203.0;
        let linear = if self.hlg {
            // HLG: luz de escena → pantalla de 1000 cd/m² con gamma de sistema 1,2.
            let [r, g, b] = [r, g, b].map(hlg_to_scene);
            let y = 0.2627 * r + 0.6780 * g + 0.0593 * b;
            let k = 1000.0 * y.max(1e-6).powf(0.2) / REFERENCE_WHITE;
            [r * k, g * k, b * k]
        } else {
            [r, g, b].map(|v| pq_to_nits(v) / REFERENCE_WHITE)
        };
        let m = &self.to_srgb;
        let mut out = [0u8; 4];
        for (c, row) in m.iter().enumerate() {
            let v = (row[0] * linear[0] + row[1] * linear[1] + row[2] * linear[2]).max(0.0) * 0.5;
            // Por encima de 0,8 se acerca a 1 sin recortar de golpe.
            let v = if v < 0.8 {
                v
            } else {
                0.8 + 0.2 * (1.0 - (-(v - 0.8) / 0.2).exp())
            };
            out[c] = (srgb_encode(v) * 255.0).round() as u8;
        }
        out[3] = (a.clamp(0.0, 1.0) * 255.0).round() as u8;
        out
    }
}

fn pq_to_nits(e: f32) -> f32 {
    let (m1, m2, c1, c2, c3) = (0.159_301_76, 78.843_75, 0.835_937_5, 18.851_563, 18.6875);
    let p = e.clamp(0.0, 1.0).powf(1.0 / m2);
    10_000.0 * ((p - c1).max(0.0) / (c2 - c3 * p)).powf(1.0 / m1)
}

fn hlg_to_scene(e: f32) -> f32 {
    let (a, b, c) = (0.178_832_77, 0.284_668_92, 0.559_910_7);
    let e = e.clamp(0.0, 1.0);
    if e <= 0.5 {
        e * e / 3.0
    } else {
        (((e - c) / a).exp() + b) / 12.0
    }
}

fn srgb_encode(v: f32) -> f32 {
    let v = v.clamp(0.0, 1.0);
    if v <= 0.003_130_8 {
        12.92 * v
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    }
}

impl DisplaySource {
    pub fn new(image: SharedImage, icc: Option<&[u8]>) -> Self {
        let to_srgb = icc
            .and_then(|icc| ColorProfile::new_from_slice(icc).ok())
            .map_or(ToSrgb::None, |p| ToSrgb::for_profile(&p));
        Self { image, to_srgb }
    }

    pub fn width(&self) -> u32 {
        self.image.get().width()
    }

    pub fn height(&self) -> u32 {
        self.image.get().height()
    }

    pub fn is_color_managed(&self) -> bool {
        !matches!(self.to_srgb, ToSrgb::None)
    }

    /// Vista completa reducida para que el lado mayor no pase de `max_side`.
    pub fn proxy(&self, max_side: u32) -> ColorImage {
        let (w, h) = (self.width(), self.height());
        let scale = (max_side as f32 / w.max(h) as f32).min(1.0);
        let (nw, nh) = (
            ((w as f32 * scale).round() as u32).max(1),
            ((h as f32 * scale).round() as u32).max(1),
        );
        if (nw, nh) == (w, h) {
            self.finish(self.image.get())
        } else {
            self.finish(&resize_native(self.image.get(), [0, 0, w, h], nw, nh))
        }
    }

    /// Recorte (`x`,`y`,`w`,`h` en píxeles de la foto) redimensionado a `out_w`×`out_h`. Si la
    /// zona se sale de la foto, se recorta y la salida se reduce en la misma proporción.
    pub fn crop(&self, x: u32, y: u32, w: u32, h: u32, out_w: u32, out_h: u32) -> ColorImage {
        let image = self.image.get();
        let (iw, ih) = (image.width(), image.height());
        if iw == 0 || ih == 0 {
            return ColorImage::filled([1, 1], egui::Color32::TRANSPARENT);
        }
        let (x, y) = (x.min(iw - 1), y.min(ih - 1));
        let (cw, ch) = (w.clamp(1, iw - x), h.clamp(1, ih - y));
        let scale_out = |out: u32, kept: u32, asked: u32| {
            ((f64::from(out) * f64::from(kept) / f64::from(asked.max(1))).round() as u32).max(1)
        };
        let (out_w, out_h) = (scale_out(out_w, cw, w), scale_out(out_h, ch, h));
        let (w, h) = (cw, ch);
        let region = if (out_w, out_h) == (w, h) {
            image.crop_imm(x, y, w, h)
        } else if image.color().has_alpha() {
            // Con alfa, fast_image_resize premultiplica toda la fuente antes de redimensionar:
            // se copia solo la zona, con un margen del tamaño del filtro para que los bordes
            // salgan igual que leyendo de la foto entera.
            let pad = (3.0 * (f64::from(w) / f64::from(out_w)).max(1.0)).ceil() as u32;
            let (px, py) = (x.saturating_sub(pad), y.saturating_sub(pad));
            let (pw, ph) = ((x + w + pad).min(iw) - px, (y + h + pad).min(ih) - py);
            let padded = image.crop_imm(px, py, pw, ph);
            resize_native(&padded, [x - px, y - py, w, h], out_w, out_h)
        } else {
            // Sin alfa se lee la zona directamente, sin copiarla antes (puede ser grande).
            resize_native(image, [x, y, w, h], out_w, out_h)
        };
        self.finish(&region)
    }

    /// Convierte a sRGB de 8 bits por franjas de filas, en paralelo y sin copias completas
    /// intermedias.
    fn finish(&self, image: &DynamicImage) -> ColorImage {
        const ROWS: u32 = 64;
        let (w, h) = (image.width(), image.height());
        let mut rgba = vec![0u8; w as usize * h as usize * 4];
        if w > 0 {
            rgba.par_chunks_mut((w * ROWS) as usize * 4)
                .enumerate()
                .for_each(|(i, out)| {
                    let y = i as u32 * ROWS;
                    self.convert(image.crop_imm(0, y, w, ROWS.min(h - y)), out);
                });
        }
        ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba)
    }

    fn convert(&self, band: DynamicImage, out: &mut [u8]) {
        match &self.to_srgb {
            ToSrgb::None => out.copy_from_slice(band.into_rgba8().as_raw()),
            ToSrgb::Rgb(transform) => {
                let src = band.into_rgba16();
                let mut dst = vec![0u16; src.len()];
                // Si la conversión fallara, se muestran los valores sin convertir.
                let converted = if transform.transform(&src, &mut dst).is_ok() {
                    &dst
                } else {
                    src.as_raw()
                };
                for (o, &v) in out.iter_mut().zip(converted) {
                    *o = to_u8(v);
                }
            }
            ToSrgb::Gray(table) => {
                let src = band.into_luma_alpha16();
                for (o, p) in out.as_chunks_mut::<4>().0.iter_mut().zip(src.pixels()) {
                    let [r, g, b] = table[usize::from(p[0])];
                    *o = [r, g, b, to_u8(p[1])];
                }
            }
            ToSrgb::Hdr(hdr) => {
                let src = band.into_rgba32f();
                for (o, p) in out.as_chunks_mut::<4>().0.iter_mut().zip(src.pixels()) {
                    *o = hdr.map(p.0);
                }
            }
        }
    }
}

/// Reduce la zona `[x, y, ancho, alto]` (en píxeles de la miniatura) de la miniatura a
/// `out_w`×`out_h` con Lanczos. Para la vista alejada basta con la miniatura y así no se recorre
/// la foto entera. La miniatura ya está en sRGB y con alfa premultiplicado (como guarda egui).
pub fn downscale(proxy: &ColorImage, zone: [f64; 4], out_w: u32, out_h: u32) -> ColorImage {
    let (pw, ph) = (proxy.size[0] as u32, proxy.size[1] as u32);
    let [x, y, w, h] = zone;
    let x = x.clamp(0.0, f64::from(pw.saturating_sub(1)));
    let y = y.clamp(0.0, f64::from(ph.saturating_sub(1)));
    let w = w.clamp(1.0, f64::from(pw) - x);
    let h = h.clamp(1.0, f64::from(ph) - y);
    let (out_w, out_h) = (out_w.max(1), out_h.max(1));
    let mut dst = FirImage::new(out_w, out_h, PixelType::U8x4);
    let options = ResizeOptions::new()
        .resize_alg(ResizeAlg::Convolution(FilterType::Lanczos3))
        .crop(x, y, w, h)
        .use_alpha(false);
    let resized = ImageRef::new(pw, ph, proxy.as_raw(), PixelType::U8x4)
        .ok()
        .and_then(|src| Resizer::new().resize(&src, &mut dst, &options).ok());
    if resized.is_none() {
        return ColorImage::filled([1, 1], egui::Color32::TRANSPARENT);
    }
    ColorImage::from_rgba_premultiplied([out_w as usize, out_h as usize], dst.buffer())
}

/// Redimensiona la zona `[x, y, ancho, alto]` en el tipo de píxel nativo (8/16 bits/float) con
/// SIMD; la conversión a sRGB de 8 bits se hace después, sin perder precisión antes.
fn resize_native(image: &DynamicImage, zone: [u32; 4], nw: u32, nh: u32) -> DynamicImage {
    let [x, y, w, h] = zone;
    let fallback = || {
        image
            .crop_imm(x, y, w, h)
            .resize_exact(nw, nh, image::imageops::FilterType::Lanczos3)
    };
    let Some(pixel_type) = image.pixel_type() else {
        return fallback();
    };
    let mut dst = FirImage::new(nw, nh, pixel_type);
    let options = ResizeOptions::new()
        .resize_alg(ResizeAlg::Convolution(FilterType::Lanczos3))
        .crop(f64::from(x), f64::from(y), f64::from(w), f64::from(h));
    if Resizer::new().resize(image, &mut dst, &options).is_err() {
        return fallback();
    }
    let bytes = dst.into_vec();
    let resized: Option<DynamicImage> = match pixel_type {
        PixelType::U8 => ImageBuffer::<Luma<u8>, _>::from_raw(nw, nh, bytes).map(Into::into),
        PixelType::U8x2 => ImageBuffer::<LumaA<u8>, _>::from_raw(nw, nh, bytes).map(Into::into),
        PixelType::U8x3 => ImageBuffer::<Rgb<u8>, _>::from_raw(nw, nh, bytes).map(Into::into),
        PixelType::U8x4 => ImageBuffer::<Rgba<u8>, _>::from_raw(nw, nh, bytes).map(Into::into),
        PixelType::U16 => {
            ImageBuffer::<Luma<u16>, _>::from_raw(nw, nh, u16s(&bytes)).map(Into::into)
        }
        PixelType::U16x2 => {
            ImageBuffer::<LumaA<u16>, _>::from_raw(nw, nh, u16s(&bytes)).map(Into::into)
        }
        PixelType::U16x3 => {
            ImageBuffer::<Rgb<u16>, _>::from_raw(nw, nh, u16s(&bytes)).map(Into::into)
        }
        PixelType::U16x4 => {
            ImageBuffer::<Rgba<u16>, _>::from_raw(nw, nh, u16s(&bytes)).map(Into::into)
        }
        PixelType::F32x3 => {
            ImageBuffer::<Rgb<f32>, _>::from_raw(nw, nh, f32s(&bytes)).map(Into::into)
        }
        PixelType::F32x4 => {
            ImageBuffer::<Rgba<f32>, _>::from_raw(nw, nh, f32s(&bytes)).map(Into::into)
        }
        _ => None,
    };
    resized.unwrap_or_else(fallback)
}

fn u16s(bytes: &[u8]) -> Vec<u16> {
    bytemuck::pod_collect_to_vec(bytes)
}

fn f32s(bytes: &[u8]) -> Vec<f32> {
    bytemuck::pod_collect_to_vec(bytes)
}
