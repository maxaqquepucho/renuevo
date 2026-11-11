//! Verificación pixel a pixel: re-decodifica lo que se va a escribir y lo compara con el origen.

use std::io::Cursor;

use image::{DynamicImage, GenericImageView, ImageDecoder, ImageFormat, ImageReader};

use crate::Result;
use crate::io::{decode_limits, tiff_icc};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifyReport {
    /// Píxeles comparados.
    pub pixels: u64,
    /// Todos los píxeles son idénticos bit a bit.
    pub identical: bool,
    /// El ICC del archivo coincide byte a byte con el esperado.
    pub icc_preserved: bool,
    /// El archivo escrito lleva un perfil ICC (si no, se interpreta como sRGB).
    pub icc_present: bool,
    /// El archivo escrito contiene EXIF (no debería).
    pub exif_present: bool,
    /// Huella BLAKE3 (16 hex) de los píxeles decodificados.
    pub fingerprint: String,
    /// Primera diferencia encontrada, si la hay.
    pub mismatch: Option<String>,
}

/// Decodifica `encoded` y lo compara con `expected` (dimensiones, tipo de píxel y cada byte).
pub fn verify_encoded(
    expected: &DynamicImage,
    expected_icc: Option<&[u8]>,
    encoded: &[u8],
) -> Result<VerifyReport> {
    let mut reader = ImageReader::new(Cursor::new(encoded)).with_guessed_format()?;
    reader.limits(decode_limits());
    let is_tiff = reader.format() == Some(ImageFormat::Tiff);
    let mut decoder = reader.into_decoder()?;
    let icc = match decoder.icc_profile()? {
        None if is_tiff => tiff_icc(encoded),
        icc => icc,
    };
    let exif = decoder.exif_metadata()?;
    let decoded = DynamicImage::from_decoder(decoder)?;

    let mismatch = first_mismatch(expected, &decoded);
    Ok(VerifyReport {
        pixels: u64::from(decoded.width()) * u64::from(decoded.height()),
        identical: mismatch.is_none(),
        icc_preserved: icc.as_deref() == expected_icc,
        icc_present: icc.is_some(),
        exif_present: exif.is_some(),
        fingerprint: fingerprint(&decoded),
        mismatch,
    })
}

fn first_mismatch(expected: &DynamicImage, actual: &DynamicImage) -> Option<String> {
    if expected.dimensions() != actual.dimensions() {
        return Some(format!(
            "tamaño {:?} en lugar de {:?}",
            actual.dimensions(),
            expected.dimensions()
        ));
    }
    if expected.color() != actual.color() {
        return Some(format!(
            "tipo de píxel {:?} en lugar de {:?}",
            actual.color(),
            expected.color()
        ));
    }
    let (a, b) = (expected.as_bytes(), actual.as_bytes());
    if a == b {
        return None;
    }
    let bytes_per_pixel = usize::from(expected.color().bytes_per_pixel()).max(1);
    let index = a.iter().zip(b).position(|(x, y)| x != y).unwrap_or(0) / bytes_per_pixel;
    let width = expected.width().max(1) as usize;
    Some(format!(
        "el píxel ({}, {}) es distinto",
        index % width,
        index / width
    ))
}

/// Huella corta de los píxeles (incluye tamaño y tipo, para que no colisionen formatos distintos).
pub fn fingerprint(image: &DynamicImage) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(&image.width().to_le_bytes());
    hasher.update(&image.height().to_le_bytes());
    hasher.update(format!("{:?}", image.color()).as_bytes());
    hasher.update(image.as_bytes());
    hasher.finalize().to_hex()[..16].to_owned()
}
