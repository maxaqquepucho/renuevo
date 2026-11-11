//! Preparar píxeles para la pantalla: miniaturas, recortes a resolución completa y conversión
//! del perfil de color de la foto a sRGB. **Solo afecta a lo que se ve**, nunca a lo exportado.

use std::sync::Arc;

use egui::ColorImage;
use fast_image_resize::images::{Image as FirImage, ImageRef};
use fast_image_resize::{FilterType, IntoImageView, PixelType, ResizeAlg, ResizeOptions, Resizer};
use image::{DynamicImage, ImageBuffer, Luma, LumaA, Rgb, Rgba, RgbaImage};
use moxcms::{ColorProfile, DataColorSpace, Layout, Transform8BitExecutor, TransformOptions};
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
    to_srgb: Option<Arc<Transform8BitExecutor>>,
}

impl DisplaySource {
    pub fn new(image: SharedImage, icc: Option<&[u8]>) -> Self {
        let to_srgb = icc.and_then(|icc| {
            let src = ColorProfile::new_from_slice(icc).ok()?;
            if src.color_space != DataColorSpace::Rgb {
                return None;
            }
            src.create_transform_8bit(
                Layout::Rgba,
                &ColorProfile::new_srgb(),
                Layout::Rgba,
                TransformOptions::default(),
            )
            .ok()
        });
        Self { image, to_srgb }
    }

    pub fn width(&self) -> u32 {
        self.image.get().width()
    }

    pub fn height(&self) -> u32 {
        self.image.get().height()
    }

    pub fn is_color_managed(&self) -> bool {
        self.to_srgb.is_some()
    }

    /// Vista completa reducida para que el lado mayor no pase de `max_side`.
    pub fn proxy(&self, max_side: u32) -> ColorImage {
        let (w, h) = (self.width(), self.height());
        let scale = (max_side as f32 / w.max(h) as f32).min(1.0);
        let (nw, nh) = (
            ((w as f32 * scale).round() as u32).max(1),
            ((h as f32 * scale).round() as u32).max(1),
        );
        let rgba = if (nw, nh) == (w, h) {
            self.image.get().to_rgba8()
        } else {
            resize_to_rgba8(self.image.get(), [0, 0, w, h], nw, nh)
        };
        self.finish(rgba)
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
        let rgba = if (out_w, out_h) == (w, h) {
            image.crop_imm(x, y, w, h).to_rgba8()
        } else if image.color().has_alpha() {
            // Con alfa, fast_image_resize premultiplica toda la fuente antes de redimensionar:
            // se copia solo la zona, con un margen del tamaño del filtro para que los bordes
            // salgan igual que leyendo de la foto entera.
            let pad = (3.0 * (f64::from(w) / f64::from(out_w)).max(1.0)).ceil() as u32;
            let (px, py) = (x.saturating_sub(pad), y.saturating_sub(pad));
            let (pw, ph) = ((x + w + pad).min(iw) - px, (y + h + pad).min(ih) - py);
            let region = image.crop_imm(px, py, pw, ph);
            resize_to_rgba8(&region, [x - px, y - py, w, h], out_w, out_h)
        } else {
            // Sin alfa se lee la zona directamente, sin copiarla antes (puede ser grande).
            resize_to_rgba8(image, [x, y, w, h], out_w, out_h)
        };
        self.finish(rgba)
    }

    fn finish(&self, mut rgba: RgbaImage) -> ColorImage {
        if let Some(transform) = &self.to_srgb {
            let src = rgba.as_raw().clone();
            // Si la conversión fallara, se muestran los valores sin convertir.
            let _ = transform.transform(&src, &mut rgba);
        }
        ColorImage::from_rgba_unmultiplied(
            [rgba.width() as usize, rgba.height() as usize],
            rgba.as_raw(),
        )
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
/// SIMD y convierte a RGBA8 al final.
fn resize_to_rgba8(image: &DynamicImage, zone: [u32; 4], nw: u32, nh: u32) -> RgbaImage {
    let [x, y, w, h] = zone;
    let fallback = || {
        image
            .crop_imm(x, y, w, h)
            .resize_exact(nw, nh, image::imageops::FilterType::Lanczos3)
            .to_rgba8()
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
    resized.map_or_else(fallback, |img| img.to_rgba8())
}

fn u16s(bytes: &[u8]) -> Vec<u16> {
    bytemuck::pod_collect_to_vec(bytes)
}

fn f32s(bytes: &[u8]) -> Vec<f32> {
    bytemuck::pod_collect_to_vec(bytes)
}
