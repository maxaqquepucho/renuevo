//! Decodificación HEIC/HEIF con libheif.
//!
//! No usamos el hook de `libheif-rs` para `image` porque copia los valores de 10/12 bits sin
//! escalar (quedarían en 0..1023 dentro de un u16, casi negro) y no expone el perfil de color.

use image::{DynamicImage, ImageBuffer};
use libheif_rs::{AuxiliaryImagesFilter, ColorSpace, HeifContext, ImageHandle, LibHeif, RgbChroma};

use super::decode::{HDR_NOTE, PREMULTIPLIED_NOTE, is_gray};
use super::{alpha, heif_boxes, icc, meta};
use crate::photo::{ColorSpaceInfo, Dpi, Photo, SourceFormat, SourceInfo};
use crate::{Error, Result};

pub(crate) fn decode(bytes: &[u8]) -> Result<Photo> {
    let ctx = HeifContext::read_from_bytes(bytes)?;
    let handle = ctx.primary_image_handle()?;

    let has_alpha = handle.has_alpha_channel();
    let preferred = handle.preferred_decoding_colorspace().ok();
    // libheif devuelve -1 (255 como u8) si la imagen no se guarda como YCbCr/monocromo.
    let source_bits = match handle.luma_bits_per_pixel() {
        bits @ 1..=16 => bits.max(8),
        _ => match preferred {
            Some(ColorSpace::Rgb(
                RgbChroma::HdrRgbBe
                | RgbChroma::HdrRgbaBe
                | RgbChroma::HdrRgbLe
                | RgbChroma::HdrRgbaLe,
            )) => 16,
            _ => 8,
        },
    };
    let high_bit_depth = source_bits > 8;

    let raw_icc = handle.color_profile_raw().map(|r| r.data);
    let nclx = handle.color_profile_nclx().map(|n| {
        (
            n.color_primaries() as u8,
            n.transfer_characteristics() as u8,
        )
    });
    // Una foto en blanco y negro se decodifica en gris (como se guardó) siempre que su perfil
    // sea de gris o sRGB; un NCLX de otro espacio necesita RGB para su ICC sintetizado.
    let monochrome = preferred == Some(ColorSpace::Monochrome)
        && match (&raw_icc, nclx) {
            (Some(icc), _) => icc::check_compatible(icc, true).is_ok(),
            (None, Some((p, t))) => icc::from_cicp(p, t).0.is_none(),
            (None, None) => true,
        };

    let mut image = if monochrome {
        decode_gray(&handle, has_alpha)?
    } else {
        decode_rgb(&handle, has_alpha, high_bit_depth)?
    };
    let mut notes = Vec::new();
    if has_alpha && handle.is_premultiplied_alpha() {
        alpha::unpremultiply(&mut image);
        notes.push(PREMULTIPLIED_NOTE.to_owned());
    }

    let (icc, color) = match (raw_icc, nclx) {
        (Some(raw), _) => icc::resolve(Some(raw), is_gray(&image)),
        (None, Some((p, t))) => icc::from_cicp(p, t),
        (None, None) => icc::resolve(None, false),
    };
    let color = match color {
        // En gris, «NCLX: sRGB» es lo mismo que no tener perfil.
        ColorSpaceInfo::Nclx { .. } if is_gray(&image) && icc.is_none() => {
            ColorSpaceInfo::AssumedSrgb
        }
        c => c,
    };

    let layout = heif_boxes::layout(bytes).unwrap_or_default();
    let gain_map = layout.has_tone_map
        || handle
            .auxiliary_images(AuxiliaryImagesFilter::OMIT_ALPHA.omit_depth())
            .iter()
            .filter_map(|aux| aux.auxiliary_type().ok())
            .any(|t| t.contains("hdrgainmap"));
    if gain_map {
        notes.push(HDR_NOTE.to_owned());
    }
    let images = ctx.image_ids().len();
    if images > 1 {
        notes.push(format!(
            "HEIF con {images} imágenes: solo se copia la principal"
        ));
    }
    if ctx.has_sequence() {
        notes.push("HEIF con secuencia (animación): solo se copia la imagen principal".to_owned());
    }

    let exif = meta::exif_summary(heif_exif(&handle).as_deref());

    Ok(Photo {
        image,
        icc,
        info: SourceInfo {
            path: None,
            format: SourceFormat::Heif,
            file_size: bytes.len() as u64,
            bits_per_channel: source_bits,
            had_exif: exif.present,
            had_gps: exif.gps,
            // libheif aplica las propiedades de giro y espejo del contenedor (irot/imir); el
            // EXIF de un HEIF no cuenta para la orientación.
            orientation_applied: layout.rotated_or_mirrored(),
            color,
            // Con un giro de 90°/270°, los DPI horizontal y vertical se intercambian.
            dpi: exif.dpi.map(|d| {
                if layout.quarter_turn() {
                    Dpi { x: d.y, y: d.x }
                } else {
                    d
                }
            }),
            notes,
        },
    })
}

fn decode_rgb(handle: &ImageHandle, has_alpha: bool, high_bit_depth: bool) -> Result<DynamicImage> {
    let chroma = match (high_bit_depth, has_alpha) {
        (false, false) => RgbChroma::Rgb,
        (false, true) => RgbChroma::Rgba,
        (true, false) => RgbChroma::HdrRgbLe,
        (true, true) => RgbChroma::HdrRgbaLe,
    };
    // libheif aplica por defecto las transformaciones del contenedor (irot/imir/clap).
    let decoded = LibHeif::new().decode(handle, ColorSpace::Rgb(chroma), None)?;
    let planes = decoded.planes();
    let plane = planes
        .interleaved
        .ok_or_else(|| Error::Unsupported("HEIF sin plano RGB entrelazado".to_owned()))?;
    let channels = if has_alpha { 4 } else { 3 };
    let samples = read_plane(&plane, channels, high_bit_depth)?;
    let (w, h) = (plane.width, plane.height);
    Ok(match (samples, has_alpha) {
        (Samples::U16(v), false) => DynamicImage::ImageRgb16(buffer(w, h, v)?),
        (Samples::U16(v), true) => DynamicImage::ImageRgba16(buffer(w, h, v)?),
        (Samples::U8(v), false) => DynamicImage::ImageRgb8(buffer(w, h, v)?),
        (Samples::U8(v), true) => DynamicImage::ImageRgba8(buffer(w, h, v)?),
    })
}

fn decode_gray(handle: &ImageHandle, has_alpha: bool) -> Result<DynamicImage> {
    let decoded = LibHeif::new().decode(handle, ColorSpace::Monochrome, None)?;
    let planes = decoded.planes();
    let y = planes
        .y
        .ok_or_else(|| Error::Unsupported("HEIF monocromo sin plano Y".to_owned()))?;
    let (w, h) = (y.width, y.height);
    // Cada plano tiene su propia profundidad (p. ej. gris de 10 bits con alfa de 8).
    let luma = read_plane(&y, 1, y.bits_per_pixel > 8)?;
    let alpha = match planes.a.filter(|_| has_alpha) {
        Some(a) if (a.width, a.height) == (w, h) => Some(read_plane(&a, 1, a.bits_per_pixel > 8)?),
        _ => None,
    };
    Ok(match (luma, alpha) {
        (Samples::U8(l), None) => DynamicImage::ImageLuma8(buffer(w, h, l)?),
        (Samples::U16(l), None) => DynamicImage::ImageLuma16(buffer(w, h, l)?),
        (Samples::U8(l), Some(Samples::U8(a))) => {
            DynamicImage::ImageLumaA8(buffer(w, h, interleave(&l, &a))?)
        }
        // Profundidades distintas: todo a 16 bits (de 8 a 16 bits es exacto y reversible).
        (l, Some(a)) => {
            let (l, a) = (l.into_u16(), a.into_u16());
            DynamicImage::ImageLumaA16(buffer(w, h, interleave(&l, &a))?)
        }
    })
}

enum Samples {
    U8(Vec<u8>),
    /// Ya escalados a 16 bits completos.
    U16(Vec<u16>),
}

impl Samples {
    fn into_u16(self) -> Vec<u16> {
        match self {
            Samples::U8(v) => v
                .into_iter()
                .map(|x| scale_to_16(u16::from(x), 8))
                .collect(),
            Samples::U16(v) => v,
        }
    }
}

/// Copia un plano sin el relleno de cada fila. Con más de 8 bits, libheif entrega u16 en
/// little-endian dentro del rango de `bits_per_pixel` (p. ej. 0..1023), que se escala a 16 bits.
fn read_plane(plane: &libheif_rs::Plane<&[u8]>, channels: usize, wide: bool) -> Result<Samples> {
    let samples_per_row = plane.width as usize * channels;
    let rows = plane
        .data
        .chunks(plane.stride.max(1))
        .take(plane.height as usize);
    let incomplete = || Error::Unsupported("fila HEIF incompleta".to_owned());
    if wide {
        let range_bits = plane.bits_per_pixel.clamp(8, 16);
        let mut out = Vec::with_capacity(samples_per_row * plane.height as usize);
        for row in rows {
            let row = row.get(..samples_per_row * 2).ok_or_else(incomplete)?;
            out.extend(
                row.as_chunks::<2>()
                    .0
                    .iter()
                    .map(|b| scale_to_16(u16::from_le_bytes(*b), range_bits)),
            );
        }
        Ok(Samples::U16(out))
    } else {
        let mut out = Vec::with_capacity(samples_per_row * plane.height as usize);
        for row in rows {
            out.extend_from_slice(row.get(..samples_per_row).ok_or_else(incomplete)?);
        }
        Ok(Samples::U8(out))
    }
}

fn interleave<T: Copy>(luma: &[T], alpha: &[T]) -> Vec<T> {
    luma.iter().zip(alpha).flat_map(|(&l, &a)| [l, a]).collect()
}

fn buffer<P: image::Pixel>(
    width: u32,
    height: u32,
    data: Vec<P::Subpixel>,
) -> Result<ImageBuffer<P, Vec<P::Subpixel>>> {
    ImageBuffer::from_raw(width, height, data)
        .ok_or_else(|| Error::Unsupported("tamaño de buffer HEIF inesperado".to_owned()))
}

/// Escala un valor de `bits` (8..=16) al rango completo de 16 bits replicando los bits altos.
/// Es reversible (`v16 >> (16 - bits)` recupera el original), así que no se pierde nada.
pub(crate) fn scale_to_16(value: u16, bits: u8) -> u16 {
    if bits >= 16 {
        return value;
    }
    let max = (1u32 << bits) - 1;
    let v = u32::from(value).min(max);
    ((v << (16 - bits)) | (v >> (2 * u32::from(bits) - 16))) as u16
}

/// Bloque EXIF de un HEIF, ya sin el prefijo de 4 bytes con el offset de la cabecera TIFF.
fn heif_exif(handle: &ImageHandle) -> Option<Vec<u8>> {
    let mut ids = [0; 4];
    let count = handle.metadata_block_ids(&mut ids, b"Exif");
    if count == 0 {
        return None;
    }
    let data = handle.metadata(ids[0]).ok()?;
    let offset = u32::from_be_bytes(data.get(..4)?.try_into().ok()?) as usize;
    data.get(4 + offset..).map(<[u8]>::to_vec)
}

#[cfg(test)]
mod tests {
    use super::scale_to_16;

    #[test]
    fn scale_10_bit_is_full_range_and_reversible() {
        assert_eq!(scale_to_16(0, 10), 0);
        assert_eq!(scale_to_16(1023, 10), u16::MAX);
        for v in 0..1024u16 {
            assert_eq!(scale_to_16(v, 10) >> 6, v);
        }
        assert_eq!(scale_to_16(4095, 12), u16::MAX);
        assert_eq!(scale_to_16(255, 8), u16::MAX);
    }
}
