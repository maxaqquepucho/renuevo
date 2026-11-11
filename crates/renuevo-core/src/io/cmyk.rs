//! CMYK (JPEG y TIFF de imprenta) → RGB con gestión de color, usando el perfil del archivo.
//!
//! `image` convierte CMYK a RGB con una fórmula ingenua que cambia mucho los colores. PNG, TIFF
//! (en nuestro codificador) y WebP no guardan CMYK, así que se convierte una sola vez, bien:
//! perfil CMYK del original → ProPhoto RGB, cuya gama contiene toda la de imprenta (Adobe RGB
//! recortaba los cian más saturados), en 16 bits para no añadir redondeos.

use image::{DynamicImage, ImageBuffer};
use moxcms::{ColorProfile, DataColorSpace, Layout, RenderingIntent, TransformOptions};
use rayon::prelude::*;
use zune_core::bytestream::ZCursor;
use zune_core::colorspace::ColorSpace;
use zune_core::options::DecoderOptions;

use super::decode::MAX_DECODE_BYTES;
use super::{icc, meta};
use crate::photo::SourceFormat;

pub(crate) struct Converted {
    pub image: DynamicImage,
    pub icc: Vec<u8>,
    pub description: String,
}

/// Muestras CMYK tal como salen del archivo; se pasan a tinta real (0 = sin tinta) y a 16 bits
/// por bloques durante la conversión, sin otra copia completa en memoria.
enum Samples {
    /// JPEG CMYK: siempre invertido (255 = sin tinta), como lo tratan macOS, libjpeg y los
    /// navegadores, lleve o no el segmento Adobe.
    JpegInverted(Vec<u8>),
    /// JPEG YCCK (Adobe): Y, Cb, Cr y K invertido.
    JpegYcck(Vec<u8>),
    U8(Vec<u8>),
    U16(Vec<u16>),
}

/// Bytes por píxel de la salida (RGB de 16 bits).
const OUT_BYTES_PER_PIXEL: u64 = 6;

pub(crate) fn convert(
    bytes: &[u8],
    format: SourceFormat,
    profile_bytes: &[u8],
) -> Result<Converted, String> {
    let profile = ColorProfile::new_from_slice(profile_bytes)
        .map_err(|e| format!("perfil CMYK ilegible: {e}"))?;
    if profile.color_space != DataColorSpace::Cmyk {
        return Err(format!("el perfil es {:?}, no CMYK", profile.color_space));
    }
    let (width, height, samples) = match format {
        SourceFormat::Jpeg => raw_jpeg(bytes)?,
        SourceFormat::Tiff => raw_tiff(bytes)?,
        _ => return Err("formato CMYK no soportado".to_owned()),
    };

    let target = ColorProfile::new_pro_photo_rgb();
    let transform = create_transform(&profile, &target)?;

    const ROWS: usize = 64;
    let w = width as usize;
    let chunk_px = w * ROWS;
    let mut rgb = vec![0u16; w * height as usize * 3];
    rgb.par_chunks_mut(chunk_px * 3)
        .enumerate()
        .try_for_each(|(i, dst)| {
            let start = i * chunk_px;
            let n = dst.len() / 3;
            let mut src = vec![0u16; n * 4];
            ink16(&samples, start, &mut src);
            transform.transform(&src, dst)
        })
        .map_err(|e| format!("no se pudo convertir el CMYK: {e}"))?;
    let image = ImageBuffer::from_raw(width, height, rgb)
        .map(DynamicImage::ImageRgb16)
        .ok_or("tamaño CMYK inesperado")?;
    let icc = icc::encode(target).ok_or("no se pudo crear el perfil ProPhoto RGB")?;
    let name = icc::describe(profile_bytes).unwrap_or_else(|| "perfil incrustado".to_owned());
    Ok(Converted {
        image,
        icc,
        description: format!("CMYK ({name}) convertido a ProPhoto RGB en 16 bits con su perfil"),
    })
}

/// Usa la intención de la cabecera del perfil, como ColorSync: los perfiles de imprenta suelen
/// declarar «perceptual», cuya tabla (A2B0) ya lleva los negros al negro de la pantalla. Si el
/// perfil no trae tabla para esa intención, se prueba con las demás.
fn create_transform(
    profile: &ColorProfile,
    target: &ColorProfile,
) -> Result<std::sync::Arc<moxcms::Transform16BitExecutor>, String> {
    let mut intents = vec![profile.rendering_intent];
    for fallback in [
        RenderingIntent::Perceptual,
        RenderingIntent::RelativeColorimetric,
    ] {
        if !intents.contains(&fallback) {
            intents.push(fallback);
        }
    }
    let mut last_error = String::new();
    for rendering_intent in intents {
        match profile.create_transform_16bit(
            Layout::Rgba,
            target,
            Layout::Rgb,
            TransformOptions {
                rendering_intent,
                ..TransformOptions::default()
            },
        ) {
            Ok(t) => return Ok(t),
            Err(e) => last_error = e.to_string(),
        }
    }
    Err(format!("no se pudo convertir el CMYK: {last_error}"))
}

/// Rellena `out` con la tinta (0 = sin tinta) en 16 bits de los píxeles `start..`.
fn ink16(samples: &Samples, start: usize, out: &mut [u16]) {
    let range = start * 4..start * 4 + out.len();
    match samples {
        Samples::JpegInverted(v) => {
            for (d, s) in out.iter_mut().zip(&v[range]) {
                *d = u16::from(255 - s) * 257;
            }
        }
        Samples::JpegYcck(v) => {
            for (d, s) in out
                .as_chunks_mut::<4>()
                .0
                .iter_mut()
                .zip(v[range].as_chunks::<4>().0)
            {
                // YCbCr → RGB da C, M y Y ya sin invertir (como libjpeg + Photoshop); K va
                // invertido. Directo a 16 bits, sin redondear antes a 8.
                let (y, cb, cr) = (
                    f32::from(s[0]),
                    f32::from(s[1]) - 128.0,
                    f32::from(s[2]) - 128.0,
                );
                let q = |v: f32| (v * 257.0).round().clamp(0.0, 65535.0) as u16;
                d[0] = q(y + 1.402 * cr);
                d[1] = q(y - 0.344_136 * cb - 0.714_136 * cr);
                d[2] = q(y + 1.772 * cb);
                d[3] = u16::from(255 - s[3]) * 257;
            }
        }
        Samples::U8(v) => {
            for (d, s) in out.iter_mut().zip(&v[range]) {
                *d = u16::from(*s) * 257;
            }
        }
        Samples::U16(v) => out.copy_from_slice(&v[range]),
    }
}

/// Comprueba antes de decodificar que caben la entrada y la salida en el límite de memoria.
fn check_memory(width: u64, height: u64, in_bytes_per_pixel: u64) -> Result<(), String> {
    let needed = width * height * (in_bytes_per_pixel + OUT_BYTES_PER_PIXEL);
    if needed > MAX_DECODE_BYTES {
        return Err(format!(
            "{width}×{height} en CMYK necesitaría {:.1} GB de memoria",
            needed as f64 / 1e9
        ));
    }
    Ok(())
}

/// Valores CMYK de un JPEG tal como están en el archivo.
fn raw_jpeg(bytes: &[u8]) -> Result<(u32, u32, Samples), String> {
    let options = DecoderOptions::default()
        .set_strict_mode(false)
        .set_max_width(usize::MAX)
        .set_max_height(usize::MAX);
    let mut decoder = zune_jpeg::JpegDecoder::new_with_options(ZCursor::new(bytes), options);
    decoder.decode_headers().map_err(|e| e.to_string())?;
    let input = decoder
        .input_colorspace()
        .ok_or("JPEG sin espacio de color")?;
    if !matches!(input, ColorSpace::CMYK | ColorSpace::YCCK) {
        return Err(format!("el JPEG es {input:?}, no CMYK"));
    }
    let (w, h) = decoder.dimensions().ok_or("JPEG sin dimensiones")?;
    check_memory(w as u64, h as u64, 4)?;
    // Pedir la salida en el mismo espacio de entrada: zune la entrega sin convertir.
    decoder.set_options(decoder.options().jpeg_set_out_colorspace(input));
    let data = decoder.decode().map_err(|e| e.to_string())?;
    if data.len() != w * h * 4 {
        return Err("datos CMYK incompletos".to_owned());
    }
    let samples = if input == ColorSpace::YCCK {
        Samples::JpegYcck(data)
    } else {
        Samples::JpegInverted(data)
    };
    Ok((w as u32, h as u32, samples))
}

/// Valores CMYK de un TIFF (`PhotometricInterpretation = Separated`, 0 = sin tinta).
fn raw_tiff(bytes: &[u8]) -> Result<(u32, u32, Samples), String> {
    use tiff::ColorType;
    use tiff::decoder::DecodingResult;

    let mut decoder = meta::tiff_decoder(bytes).ok_or("TIFF ilegible")?;
    let (w, h) = decoder.dimensions().map_err(|e| e.to_string())?;
    let bytes_per_sample = match decoder.colortype().map_err(|e| e.to_string())? {
        ColorType::CMYK(8) => 1,
        ColorType::CMYK(16) => 2,
        other => return Err(format!("TIFF {other:?} no soportado")),
    };
    check_memory(u64::from(w), u64::from(h), 4 * bytes_per_sample)?;
    let samples = match decoder.read_image().map_err(|e| e.to_string())? {
        DecodingResult::U8(v) => Samples::U8(v),
        DecodingResult::U16(v) => Samples::U16(v),
        _ => return Err("muestras CMYK de tipo inesperado".to_owned()),
    };
    let len = match &samples {
        Samples::U8(v) => v.len(),
        Samples::U16(v) => v.len(),
        _ => 0,
    };
    if len != w as usize * h as usize * 4 {
        return Err("datos CMYK incompletos".to_owned());
    }
    Ok((w, h, samples))
}

#[cfg(test)]
mod tests {
    use super::check_memory;

    #[test]
    fn huge_cmyk_is_refused_before_decoding() {
        assert!(check_memory(8000, 6000, 4).is_ok());
        assert!(check_memory(50_000, 50_000, 4).is_err());
    }
}
