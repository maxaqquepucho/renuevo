//! Guardar sin pérdida: PNG (8/16 bits), TIFF (8/16 bits y float) y WebP lossless (8 bits).

use std::borrow::Cow;
use std::io::{Cursor, Seek, Write};
use std::path::Path;

use image::codecs::png::{CompressionType, FilterType as PngFilter, PngEncoder};
use image::codecs::webp::WebPEncoder;
use image::{ColorType, DynamicImage, GenericImageView, ImageEncoder, ImageError};
use tiff::encoder::{Rational, TiffEncoder, TiffValue, colortype};
use tiff::tags::{ResolutionUnit, Tag, Type};

use super::meta;
use crate::photo::Dpi;
use crate::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OutputFormat {
    #[default]
    Png,
    Tiff,
    WebP,
}

impl OutputFormat {
    pub const ALL: [OutputFormat; 3] = [Self::Png, Self::Tiff, Self::WebP];

    pub fn extension(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Tiff => "tiff",
            Self::WebP => "webp",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Png => "PNG (sin pérdida, hasta 16 bits)",
            Self::Tiff => "TIFF (sin pérdida, sin compresión)",
            Self::WebP => "WebP sin pérdida (solo 8 bits)",
        }
    }

    /// ¿Puede guardar exactamente este tipo de píxel sin convertirlo?
    pub fn can_store(self, color: ColorType) -> bool {
        use ColorType::*;
        match self {
            Self::Png => matches!(color, L8 | La8 | Rgb8 | Rgba8 | L16 | La16 | Rgb16 | Rgba16),
            Self::Tiff => matches!(
                color,
                L8 | Rgb8 | Rgba8 | L16 | Rgb16 | Rgba16 | Rgb32F | Rgba32F
            ),
            // El decodificador WebP siempre devuelve RGB(A): un gris no volvería idéntico.
            Self::WebP => matches!(color, Rgb8 | Rgba8),
        }
    }

    /// Formato recomendado para guardar exactamente este tipo de píxel.
    pub fn suggestion_for(color: ColorType) -> &'static str {
        if Self::Png.can_store(color) {
            "PNG o TIFF"
        } else {
            "TIFF"
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PngCompression {
    Fast,
    #[default]
    Balanced,
    Max,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct EncodeOptions {
    pub format: OutputFormat,
    pub png_compression: PngCompression,
    /// Resolución de impresión que se anota en PNG (`pHYs`) y TIFF. WebP no tiene dónde.
    pub dpi: Option<Dpi>,
}

/// Codifica en memoria. Falla en lugar de convertir si el formato no puede guardar los píxeles
/// exactamente (p. ej. 16 bits en WebP), para no perder calidad en silencio.
pub fn encode(
    image: &DynamicImage,
    icc: Option<&[u8]>,
    options: &EncodeOptions,
) -> Result<Vec<u8>> {
    let color = image.color();
    if !options.format.can_store(color) {
        return Err(Error::Unsupported(format!(
            "{} no puede guardar píxeles {color:?} sin pérdida; usa {}",
            options.format.extension().to_uppercase(),
            OutputFormat::suggestion_for(color)
        )));
    }

    // Solo DPI con sentido (los campos de `Dpi` son públicos y podrían traer NaN o negativos).
    let dpi = options.dpi.and_then(|d| Dpi::new(d.x, d.y));
    let mut buf = Vec::new();
    match options.format {
        OutputFormat::Png => {
            let compression = match options.png_compression {
                PngCompression::Fast => CompressionType::Fast,
                PngCompression::Balanced => CompressionType::Default,
                PngCompression::Max => CompressionType::Best,
            };
            let mut encoder =
                PngEncoder::new_with_quality(&mut buf, compression, PngFilter::Adaptive);
            set_icc(&mut encoder, icc)?;
            image.write_with_encoder(encoder)?;
            if let Some(dpi) = dpi {
                buf = meta::png_with_dpi(buf, dpi);
            }
        }
        OutputFormat::Tiff => encode_tiff(image, icc, dpi, &mut buf)?,
        OutputFormat::WebP => {
            let mut encoder = WebPEncoder::new_lossless(&mut buf);
            set_icc(&mut encoder, icc)?;
            image.write_with_encoder(encoder)?;
        }
    }
    Ok(buf)
}

/// TIFF con la crate `tiff` directamente (el codificador de `image` no escribe DPI ni declara
/// el canal alfa con `ExtraSamples`, y algunos programas lo ignoran sin esa etiqueta).
fn encode_tiff(
    image: &DynamicImage,
    icc: Option<&[u8]>,
    dpi: Option<Dpi>,
    buf: &mut Vec<u8>,
) -> Result<()> {
    let mut encoder = TiffEncoder::new(Cursor::new(buf)).map_err(tiff_error)?;
    let (w, h) = image.dimensions();
    let meta = TiffMeta { icc, dpi };
    match image {
        DynamicImage::ImageLuma8(b) => {
            write_tiff::<colortype::Gray8, _>(&mut encoder, w, h, b, &meta, false)
        }
        DynamicImage::ImageRgb8(b) => {
            write_tiff::<colortype::RGB8, _>(&mut encoder, w, h, b, &meta, false)
        }
        DynamicImage::ImageRgba8(b) => {
            write_tiff::<colortype::RGBA8, _>(&mut encoder, w, h, b, &meta, true)
        }
        DynamicImage::ImageLuma16(b) => {
            write_tiff::<colortype::Gray16, _>(&mut encoder, w, h, b, &meta, false)
        }
        DynamicImage::ImageRgb16(b) => {
            write_tiff::<colortype::RGB16, _>(&mut encoder, w, h, b, &meta, false)
        }
        DynamicImage::ImageRgba16(b) => {
            write_tiff::<colortype::RGBA16, _>(&mut encoder, w, h, b, &meta, true)
        }
        DynamicImage::ImageRgb32F(b) => {
            write_tiff::<colortype::RGB32Float, _>(&mut encoder, w, h, b, &meta, false)
        }
        DynamicImage::ImageRgba32F(b) => {
            write_tiff::<colortype::RGBA32Float, _>(&mut encoder, w, h, b, &meta, true)
        }
        other => Err(Error::Unsupported(format!(
            "TIFF no puede guardar píxeles {:?}",
            other.color()
        ))),
    }
}

struct TiffMeta<'a> {
    icc: Option<&'a [u8]>,
    dpi: Option<Dpi>,
}

fn write_tiff<C, W>(
    encoder: &mut TiffEncoder<W>,
    width: u32,
    height: u32,
    data: &[C::Inner],
    meta: &TiffMeta,
    alpha: bool,
) -> Result<()>
where
    C: colortype::ColorType,
    W: Write + Seek,
    [C::Inner]: TiffValue,
{
    let mut image = encoder.new_image::<C>(width, height).map_err(tiff_error)?;
    if let Some(icc) = meta.icc {
        image
            .encoder()
            .write_tag(Tag::IccProfile, Undefined(icc))
            .map_err(tiff_error)?;
    }
    if alpha {
        // 2 = alfa no asociado (no premultiplicado), que es lo que guardamos.
        image
            .encoder()
            .write_tag(Tag::ExtraSamples, 2u16)
            .map_err(tiff_error)?;
    }
    // La crate escribe siempre 1/1 «sin unidad», que macOS lee como 1 ppp: sin DPI de origen se
    // anotan 72 ppp, lo que los programas suponen cuando falta el dato (y lo que da un PNG).
    let dpi = meta.dpi.unwrap_or(Dpi { x: 72.0, y: 72.0 });
    image.resolution_unit(ResolutionUnit::Inch);
    image.x_resolution(rational(dpi.x));
    image.y_resolution(rational(dpi.y));
    image.write_data(data).map_err(tiff_error)
}

/// El ICC va en TIFF como `UNDEFINED` (tipo 7), como pide la especificación ICC; un slice de
/// bytes se escribiría como `BYTE`.
struct Undefined<'a>(&'a [u8]);

impl TiffValue for Undefined<'_> {
    const BYTE_LEN: u8 = 1;
    const FIELD_TYPE: Type = Type::UNDEFINED;

    fn count(&self) -> usize {
        self.0.len()
    }

    fn data(&self) -> Cow<'_, [u8]> {
        Cow::Borrowed(self.0)
    }
}

fn rational(v: f64) -> Rational {
    // 299,9994 (lo que da un pHYs de PNG) es en realidad 300.
    if (v - v.round()).abs() < 0.01 && v <= f64::from(u32::MAX) {
        Rational {
            n: v.round() as u32,
            d: 1,
        }
    } else {
        Rational {
            n: (v * 1000.0).round().min(f64::from(u32::MAX)) as u32,
            d: 1000,
        }
    }
}

fn tiff_error(e: tiff::TiffError) -> Error {
    Error::Unsupported(format!("no se pudo escribir el TIFF: {e}"))
}

fn set_icc(encoder: &mut impl ImageEncoder, icc: Option<&[u8]>) -> Result<()> {
    if let Some(icc) = icc {
        encoder
            .set_icc_profile(icc.to_vec())
            .map_err(|e| Error::Image(ImageError::Unsupported(e)))?;
    }
    Ok(())
}

/// Escribe de forma atómica: a un temporal único junto al destino, `fsync`, y `rename`.
/// Si algo falla, el temporal se borra solo y el destino no se toca.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let dir = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    let mut tmp = tempfile::Builder::new()
        .prefix(".renuevo-")
        .suffix(".tmp")
        .tempfile_in(dir)?;
    tmp.write_all(bytes)?;
    tmp.as_file().sync_all()?;
    tmp.persist(path).map_err(|e| Error::Io(e.error))?;
    Ok(())
}
