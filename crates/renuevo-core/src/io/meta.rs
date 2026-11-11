//! Metadatos del contenedor que `image` no expone: DPI, EXIF/GPS y alfa asociado de TIFF,
//! páginas o fotogramas extra, y mapas de ganancia HDR. Solo se leen para informar o para
//! conservar lo que sí cabe en la salida (perfil, DPI); el EXIF nunca se copia.

use std::io::Cursor;

use tiff::decoder::ifd::Value;
use tiff::tags::Tag;

use crate::photo::Dpi;

/// Lo que nos interesa del EXIF antes de descartarlo.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub(crate) struct ExifSummary {
    pub present: bool,
    pub gps: bool,
    pub dpi: Option<Dpi>,
}

/// Acepta el bloque con o sin la cabecera `Exif\0\0`.
pub(crate) fn exif_summary(exif: Option<&[u8]>) -> ExifSummary {
    let Some(raw) = exif else {
        return ExifSummary::default();
    };
    let tiff = raw.strip_prefix(b"Exif\0\0").unwrap_or(raw);
    let Ok(parsed) = exif::Reader::new().read_raw(tiff.to_vec()) else {
        return ExifSummary {
            present: true,
            ..ExifSummary::default()
        };
    };
    let gps = parsed
        .fields()
        .any(|f| f.tag.context() == exif::Context::Gps);
    let rational = |tag| match parsed.get_field(tag, exif::In::PRIMARY).map(|f| &f.value) {
        Some(exif::Value::Rational(v)) => v.first().map(exif::Rational::to_f64),
        _ => None,
    };
    let unit = parsed
        .get_field(exif::Tag::ResolutionUnit, exif::In::PRIMARY)
        .and_then(|f| f.value.get_uint(0))
        .unwrap_or(2);
    let dpi = match (
        rational(exif::Tag::XResolution),
        rational(exif::Tag::YResolution),
    ) {
        (Some(x), Some(y)) => resolution_to_dpi(x, y, unit),
        _ => None,
    };
    ExifSummary {
        present: true,
        gps,
        dpi,
    }
}

/// Unidades de TIFF/EXIF: 2 = pulgada, 3 = centímetro; 1 (sin unidad) no da DPI.
fn resolution_to_dpi(x: f64, y: f64, unit: u32) -> Option<Dpi> {
    match unit {
        2 => Dpi::new(x, y),
        3 => Dpi::new(x * 2.54, y * 2.54),
        _ => None,
    }
}

// ---- JPEG ----

#[derive(Debug, Default, Clone, PartialEq)]
pub(crate) struct JpegMeta {
    /// Densidad de la cabecera JFIF (si declara unidades reales).
    pub jfif_dpi: Option<Dpi>,
    /// Posición en el archivo y valor del byte «transform» del segmento APP14 «Adobe».
    pub adobe_transform: Option<(usize, u8)>,
    /// Factores de muestreo (horizontal, vertical) de cada componente del marcador SOF.
    pub sampling: Vec<(u8, u8)>,
    /// Los datos de imagen terminan con el marcador EOI (si no, el archivo está truncado).
    pub complete: bool,
}

impl JpegMeta {
    /// Componentes de color (4 = CMYK/YCCK).
    pub fn components(&self) -> usize {
        self.sampling.len()
    }

    /// zune-jpeg 0.5 solo decodifica bien el submuestreo si el primer componente es el de más
    /// resolución (lo normal: 1×1 en todos, o YCC(K) con Y a 2×2). Con otro reparto, p. ej. K a
    /// 2×2 y C, M, Y a 1×1, devuelve colores erróneos sin avisar.
    pub fn sampling_supported(&self) -> bool {
        let Some(&(h0, v0)) = self.sampling.first() else {
            return true;
        };
        self.sampling.iter().all(|&(h, v)| h <= h0 && v <= v0)
    }
}

pub(crate) fn jpeg_meta(bytes: &[u8]) -> JpegMeta {
    let mut meta = JpegMeta::default();
    let (segments, scan_start) = jpeg_segments(bytes);
    for (offset, marker, data) in segments {
        match marker {
            0xE0 if data.starts_with(b"JFIF\0") && data.len() >= 12 => {
                let x = f64::from(u16::from_be_bytes([data[8], data[9]]));
                let y = f64::from(u16::from_be_bytes([data[10], data[11]]));
                meta.jfif_dpi = match data[7] {
                    1 => Dpi::new(x, y),
                    2 => Dpi::new(x * 2.54, y * 2.54),
                    _ => None,
                };
            }
            // «Adobe», versión (2), flags0 (2), flags1 (2) y transform (1).
            0xEE if data.starts_with(b"Adobe") && data.len() >= 12 => {
                meta.adobe_transform = Some((offset + 11, data[11]));
            }
            // SOF0..SOF15 salvo DHT (C4), JPG (C8) y DAC (CC).
            0xC0..=0xCF if !matches!(marker, 0xC4 | 0xC8 | 0xCC) && data.len() >= 6 => {
                let n = usize::from(data[5]);
                meta.sampling = data
                    .get(6..6 + 3 * n)
                    .unwrap_or_default()
                    .as_chunks::<3>()
                    .0
                    .iter()
                    .map(|c| (c[1] >> 4, c[1] & 0x0F))
                    .collect();
            }
            _ => {}
        }
    }
    // Dentro de los datos comprimidos, 0xFF siempre va seguido de 0x00 o de un RST: un FF D9
    // solo puede ser el final de la imagen.
    meta.complete =
        scan_start.is_some_and(|start| bytes[start..].windows(2).any(|w| w == [0xFF, 0xD9]));
    meta
}

/// Un APP14 «Adobe» con un valor de `transform` desconocido (>2) hace que zune rechace todo el
/// archivo, aunque macOS lo abre. Devuelve una copia con el valor corregido (0 = sin conversión
/// para CMYK, 1 = YCbCr para 3 componentes), o `None` si no hace falta.
pub(crate) fn jpeg_with_known_adobe_transform(bytes: &[u8], meta: &JpegMeta) -> Option<Vec<u8>> {
    let (offset, transform) = meta.adobe_transform?;
    if transform <= 2 {
        return None;
    }
    let mut fixed = bytes.to_vec();
    *fixed.get_mut(offset)? = if meta.components() == 4 { 0 } else { 1 };
    Some(fixed)
}

/// Un segmento de cabecera: posición de sus datos en el archivo, marcador y datos.
type Segment<'a> = (usize, u8, &'a [u8]);

/// Segmentos de la cabecera de un JPEG hasta el primer SOS, y dónde empiezan los datos de
/// imagen.
fn jpeg_segments(bytes: &[u8]) -> (Vec<Segment<'_>>, Option<usize>) {
    let mut out = Vec::new();
    if !bytes.starts_with(&[0xFF, 0xD8]) {
        return (out, None);
    }
    let mut i = 2;
    while i + 4 <= bytes.len() && bytes[i] == 0xFF {
        let marker = bytes[i + 1];
        if marker == 0xFF {
            i += 1; // relleno
            continue;
        }
        if marker == 0xD9 {
            break;
        }
        let len = usize::from(u16::from_be_bytes([bytes[i + 2], bytes[i + 3]])).max(2);
        if marker == 0xDA {
            return (out, Some((i + 2 + len).min(bytes.len())));
        }
        let Some(data) = bytes.get(i + 4..i + 2 + len) else {
            break;
        };
        out.push((i + 4, marker, data));
        i += 2 + len;
    }
    (out, None)
}

/// Mapa de ganancia HDR (Ultra HDR / ISO 21496 o el de Apple) en un JPEG: va en una imagen
/// secundaria que se describe con XMP. Basta con buscar sus nombres en el archivo.
pub(crate) fn jpeg_has_gain_map(bytes: &[u8]) -> bool {
    contains(bytes, b"hdrgm:") || contains(bytes, b"hdrgainmap") || contains(bytes, b"HDRGainMap")
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

// ---- PNG ----

#[derive(Debug, Default, Clone, PartialEq)]
pub(crate) struct PngMeta {
    pub cicp: Option<(u8, u8)>,
    pub srgb: bool,
    /// Valor del chunk `gAMA` (gamma de codificación, p. ej. 0,45455).
    pub gamma: Option<f32>,
    /// `cHRM`: punto blanco y primarias (x, y).
    pub chromaticities: Option<[(f32, f32); 4]>,
    pub dpi: Option<Dpi>,
    /// Fotogramas si es un PNG animado (APNG).
    pub frames: Option<u32>,
}

pub(crate) fn png_meta(bytes: &[u8]) -> Option<PngMeta> {
    let reader = png::Decoder::new(Cursor::new(bytes)).read_info().ok()?;
    let info = reader.info();
    // png 0.18 guarda los chunks leídos en `gama_chunk`/`chrm_chunk` (los campos `source_*`
    // son para codificar).
    let chromaticities = info
        .chrm_chunk
        .map(|c| [c.white, c.red, c.green, c.blue].map(|(x, y)| (x.into_value(), y.into_value())));
    // `pHYs` va en píxeles por metro (enteros): 300 ppp se guardan como 11811 → 299,9994.
    let to_dpi = |ppm: u32| {
        let v = f64::from(ppm) * 0.0254;
        if (v - v.round()).abs() < 0.02 {
            v.round()
        } else {
            v
        }
    };
    let dpi = info.pixel_dims.and_then(|d| match d.unit {
        png::Unit::Meter => Dpi::new(to_dpi(d.xppu), to_dpi(d.yppu)),
        png::Unit::Unspecified => None,
    });
    Some(PngMeta {
        cicp: info
            .coding_independent_code_points
            .map(|c| (c.color_primaries, c.transfer_function)),
        srgb: info.srgb.is_some(),
        gamma: info.gama_chunk.map(|g| g.into_value()),
        chromaticities,
        dpi,
        frames: info
            .animation_control
            .map(|a| a.num_frames)
            .filter(|&n| n > 1),
    })
}

/// Inserta un chunk `pHYs` justo después de `IHDR` (debe ir antes de `IDAT`).
pub(crate) fn png_with_dpi(png: Vec<u8>, dpi: Dpi) -> Vec<u8> {
    const IHDR_END: usize = 8 + 8 + 13 + 4;
    if png.len() < IHDR_END || &png[12..16] != b"IHDR" {
        return png;
    }
    let ppm = |v: f64| (v / 0.0254).round().clamp(1.0, f64::from(u32::MAX)) as u32;
    let mut chunk = Vec::with_capacity(21);
    chunk.extend_from_slice(&9u32.to_be_bytes());
    chunk.extend_from_slice(b"pHYs");
    chunk.extend_from_slice(&ppm(dpi.x).to_be_bytes());
    chunk.extend_from_slice(&ppm(dpi.y).to_be_bytes());
    chunk.push(1); // unidad: metro
    let crc = crc32fast::hash(&chunk[4..]);
    chunk.extend_from_slice(&crc.to_be_bytes());

    let mut out = Vec::with_capacity(png.len() + chunk.len());
    out.extend_from_slice(&png[..IHDR_END]);
    out.extend_from_slice(&chunk);
    out.extend_from_slice(&png[IHDR_END..]);
    out
}

// ---- WebP ----

/// Bandera de animación de la cabecera extendida `VP8X`.
pub(crate) fn webp_is_animated(bytes: &[u8]) -> bool {
    bytes.len() > 20 && &bytes[12..16] == b"VP8X" && bytes[20] & 0x02 != 0
}

// ---- TIFF ----

#[derive(Debug, Default, Clone, PartialEq)]
pub(crate) struct TiffMeta {
    pub icc: Option<Vec<u8>>,
    /// `ExtraSamples = 1`: el alfa está premultiplicado (asociado).
    pub associated_alpha: bool,
    pub exif: bool,
    pub gps: bool,
    pub dpi: Option<Dpi>,
    pub pages: usize,
}

pub(crate) fn tiff_decoder(bytes: &[u8]) -> Option<tiff::decoder::Decoder<Cursor<&[u8]>>> {
    let mut limits = tiff::decoder::Limits::default();
    // Un ICC o un bloque de etiquetas puede pasar de 1 MiB (el valor por defecto).
    limits.ifd_value_size = 64 * 1024 * 1024;
    limits.decoding_buffer_size = usize::try_from(super::decode::MAX_DECODE_BYTES).ok()?;
    Some(
        tiff::decoder::Decoder::new(Cursor::new(bytes))
            .ok()?
            .with_limits(limits),
    )
}

pub(crate) fn tiff_meta(bytes: &[u8]) -> TiffMeta {
    let Some(mut d) = tiff_decoder(bytes) else {
        return TiffMeta::default();
    };
    let has = |d: &mut tiff::decoder::Decoder<_>, tag| matches!(d.find_tag(tag), Ok(Some(_)));
    let icc = d.get_tag_u8_vec(Tag::IccProfile).ok();
    let associated_alpha = d
        .find_tag_unsigned_vec::<u16>(Tag::ExtraSamples)
        .ok()
        .flatten()
        .is_some_and(|v| v.first() == Some(&1));
    let exif = has(&mut d, Tag::ExifDirectory);
    let gps = has(&mut d, Tag::GpsDirectory);
    let unit = d
        .find_tag_unsigned::<u32>(Tag::ResolutionUnit)
        .ok()
        .flatten()
        .unwrap_or(2);
    let mut rational = |tag| match d.find_tag(tag) {
        Ok(Some(Value::Rational(n, den))) if den != 0 => Some(f64::from(n) / f64::from(den)),
        Ok(Some(v)) => v.into_u32().ok().map(f64::from),
        _ => None,
    };
    let dpi = match (rational(Tag::XResolution), rational(Tag::YResolution)) {
        (Some(x), Some(y)) => resolution_to_dpi(x, y, unit),
        _ => None,
    };
    let mut pages = 1;
    while d.more_images() && pages < 10_000 {
        if d.next_image().is_err() {
            break;
        }
        pages += 1;
    }
    TiffMeta {
        icc,
        associated_alpha,
        exif: exif || gps,
        gps,
        dpi,
        pages,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jfif_density_in_dpi_and_dpcm() {
        let jfif = |units: u8, x: u16, y: u16| {
            let mut v = vec![0xFF, 0xD8, 0xFF, 0xE0, 0, 16];
            v.extend_from_slice(b"JFIF\0\x01\x02");
            v.push(units);
            v.extend_from_slice(&x.to_be_bytes());
            v.extend_from_slice(&y.to_be_bytes());
            v.extend_from_slice(&[0, 0, 0xFF, 0xD9]);
            v
        };
        assert_eq!(
            jpeg_meta(&jfif(1, 300, 300)).jfif_dpi,
            Dpi::new(300.0, 300.0)
        );
        let cm = jpeg_meta(&jfif(2, 100, 100)).jfif_dpi.unwrap();
        assert!((cm.x - 254.0).abs() < 1e-9);
        // Unidades 0 = solo proporción: no hay DPI.
        assert_eq!(jpeg_meta(&jfif(0, 1, 1)).jfif_dpi, None);
    }

    #[test]
    fn webp_animation_flag() {
        let mut v = b"RIFF\0\0\0\0WEBPVP8X\x0a\0\0\0".to_vec();
        v.extend_from_slice(&[0x02, 0, 0, 0]);
        assert!(webp_is_animated(&v));
        v[20] = 0x20; // solo ICC
        assert!(!webp_is_animated(&v));
    }
}
