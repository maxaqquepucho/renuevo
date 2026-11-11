//! Abrir fotos: JPEG, PNG, WebP y TIFF con `image`; HEIC/HEIF con libheif.

use std::io::Cursor;
use std::path::Path;

use image::metadata::Orientation;
use image::{DynamicImage, ExtendedColorType, ImageDecoder, ImageFormat, ImageReader, Limits};

use super::{alpha, cmyk, icc, meta};
use crate::photo::{ColorSpaceInfo, Dpi, Photo, SourceFormat, SourceInfo};
use crate::{Error, Result};

/// Memoria máxima que puede reservar un decodificador (el valor por defecto de `image` son 512 MiB,
/// insuficiente para TIFF de 16 bits grandes).
pub(crate) const MAX_DECODE_BYTES: u64 = 8 * 1024 * 1024 * 1024;

pub(crate) fn decode_limits() -> Limits {
    let mut limits = Limits::default();
    limits.max_alloc = Some(MAX_DECODE_BYTES);
    limits
}

/// Abre una foto desde disco.
pub fn open(path: impl AsRef<Path>) -> Result<Photo> {
    let path = path.as_ref();
    let bytes = std::fs::read(path)?;
    let mut photo = decode_bytes(&bytes)?;
    photo.info.path = Some(path.to_path_buf());
    Ok(photo)
}

/// Decodifica una foto en memoria. Aplica la orientación a los píxeles, conserva el ICC
/// y descarta el EXIF (solo se anota si había EXIF/GPS).
pub fn decode_bytes(bytes: &[u8]) -> Result<Photo> {
    if is_heif(bytes) {
        #[cfg(feature = "heic")]
        return super::heic::decode(bytes);
        #[cfg(not(feature = "heic"))]
        return Err(Error::Unsupported(
            "HEIC/HEIF (compila con la feature `heic`)".to_owned(),
        ));
    }
    decode_with_image_crate(bytes)
}

/// Detecta contenedores HEIF por la caja `ftyp` (major brand).
pub(crate) fn is_heif(bytes: &[u8]) -> bool {
    bytes.len() >= 12
        && &bytes[4..8] == b"ftyp"
        && matches!(
            &bytes[8..12],
            b"heic" | b"heix" | b"hevc" | b"hevx" | b"heim" | b"heis" | b"mif1" | b"mif2" | b"msf1"
        )
}

fn decode_with_image_crate(original: &[u8]) -> Result<Photo> {
    let jpeg = original
        .starts_with(&[0xFF, 0xD8])
        .then(|| meta::jpeg_meta(original));
    if let Some(j) = &jpeg
        && j.components() == 4
        && !j.sampling_supported()
    {
        return Err(Error::Unsupported(
            "JPEG CMYK con un submuestreo poco habitual (la K con más resolución que C, M y Y): \
             el decodificador lo leería con colores erróneos"
                .to_owned(),
        ));
    }
    let patched = jpeg
        .as_ref()
        .and_then(|j| meta::jpeg_with_known_adobe_transform(original, j));
    let bytes = patched.as_deref().unwrap_or(original);

    let mut reader = ImageReader::new(Cursor::new(bytes)).with_guessed_format()?;
    reader.limits(decode_limits());
    let format = match reader.format() {
        Some(ImageFormat::Jpeg) => SourceFormat::Jpeg,
        Some(ImageFormat::Png) => SourceFormat::Png,
        Some(ImageFormat::WebP) => SourceFormat::WebP,
        Some(ImageFormat::Tiff) => SourceFormat::Tiff,
        Some(_) => SourceFormat::Other,
        None => {
            return Err(Error::Unsupported(
                "no se reconoce el formato del archivo".to_owned(),
            ));
        }
    };

    let mut decoder = reader.into_decoder()?;
    // EXIF primero: JPEG y WebP guardan la orientación al leerlo y no repiten el análisis.
    let exif = decoder.exif_metadata()?;
    let orientation = decoder.orientation()?;
    let tiff = (format == SourceFormat::Tiff).then(|| meta::tiff_meta(bytes));
    let icc = match decoder.icc_profile()? {
        None => tiff.as_ref().and_then(|t| t.icc.clone()),
        icc => icc,
    };
    let original_color = decoder.original_color_type();
    let bits_per_channel = bits_per_channel(original_color);
    // `image` declara los JPEG CMYK como RGB (ya los convierte él): se mira el marcador SOF.
    let is_cmyk = matches!(
        original_color,
        ExtendedColorType::Cmyk8 | ExtendedColorType::Cmyk16
    ) || jpeg.as_ref().is_some_and(|j| j.components() == 4);

    let mut notes = Vec::new();
    let exif_info = meta::exif_summary(exif.as_deref());
    let mut dpi = exif_info.dpi;
    let (mut had_exif, mut had_gps) = (exif_info.present, exif_info.gps);

    // CMYK con su perfil: conversión con gestión de color en lugar de la fórmula de `image`.
    let cmyk = if is_cmyk && let Some(profile) = &icc {
        Some(cmyk::convert(bytes, format, profile))
    } else {
        None
    };
    let (mut image, icc, color) = match cmyk {
        Some(Ok(c)) => {
            notes.push(
                "CMYK de imprenta: se guarda en RGB (PNG/TIFF/WebP no admiten CMYK) con los mismos \
                 colores, convertidos con su perfil"
                    .to_owned(),
            );
            (
                c.image,
                Some(c.icc),
                ColorSpaceInfo::Converted {
                    description: c.description,
                },
            )
        }
        failed => {
            let image = DynamicImage::from_decoder(decoder)?;
            let gray = is_gray(&image);
            let (icc, color) = if is_cmyk {
                // Sin perfil (o con uno que no se pudo usar) solo queda la fórmula de `image`:
                // los píxeles ya no son los originales y se avisa.
                let why = match failed {
                    Some(Err(e)) => e,
                    _ => "sin perfil CMYK".to_owned(),
                };
                (
                    None,
                    ColorSpaceInfo::Discarded {
                        reason: format!(
                            "CMYK convertido a RGB sin gestión de color ({why}): los colores pueden cambiar"
                        ),
                    },
                )
            } else if icc.is_none() && format == SourceFormat::Png {
                png_color(bytes, gray)
            } else {
                icc::resolve(icc, gray)
            };
            (image, icc, color)
        }
    };

    match format {
        SourceFormat::Jpeg => {
            if let Some(j) = &jpeg {
                dpi = dpi.or(j.jfif_dpi);
                if !j.complete {
                    notes.push(
                        "El JPEG no termina como debe (¿archivo cortado?): la parte que falta \
                         puede salir gris"
                            .to_owned(),
                    );
                }
            }
            if meta::jpeg_has_gain_map(bytes) {
                notes.push(HDR_NOTE.to_owned());
            }
        }
        SourceFormat::Png => {
            if let Some(png) = meta::png_meta(bytes) {
                dpi = png.dpi.or(dpi);
                if let Some(frames) = png.frames {
                    notes.push(format!(
                        "PNG animado ({frames} fotogramas): solo se copia el primero"
                    ));
                }
            }
        }
        SourceFormat::WebP if meta::webp_is_animated(bytes) => {
            notes.push("WebP animado: solo se copia el primer fotograma".to_owned());
        }
        SourceFormat::Tiff => {
            if let Some(t) = &tiff {
                dpi = t.dpi.or(dpi);
                had_exif |= t.exif;
                had_gps |= t.gps;
                if t.associated_alpha && image.color().has_alpha() {
                    alpha::unpremultiply(&mut image);
                    notes.push(PREMULTIPLIED_NOTE.to_owned());
                }
                if t.pages > 1 {
                    notes.push(format!(
                        "TIFF con {} páginas: solo se copia la primera",
                        t.pages
                    ));
                }
            }
        }
        _ => {}
    }

    let orientation_applied = orientation != Orientation::NoTransforms;
    image.apply_orientation(orientation);
    // Al girar 90°/270° también se intercambian los DPI horizontal y vertical.
    if matches!(
        orientation,
        Orientation::Rotate90
            | Orientation::Rotate270
            | Orientation::Rotate90FlipH
            | Orientation::Rotate270FlipH
    ) {
        dpi = dpi.map(|d| Dpi { x: d.y, y: d.x });
    }

    Ok(Photo {
        image,
        icc,
        info: SourceInfo {
            path: None,
            format,
            file_size: bytes.len() as u64,
            bits_per_channel,
            had_exif,
            had_gps,
            orientation_applied,
            color,
            dpi,
            notes,
        },
    })
}

pub(crate) const HDR_NOTE: &str = "Foto HDR con mapa de ganancia: se copia la imagen normal (SDR); el brillo extra HDR no \
     cabe en PNG/TIFF/WebP";
pub(crate) const PREMULTIPLIED_NOTE: &str =
    "Alfa premultiplicado convertido a alfa normal (el que guardan PNG/TIFF/WebP)";

/// Espacio de color de un PNG sin `iCCP`: `cICP` y `gAMA`/`cHRM` se convierten en un ICC
/// equivalente (nuestro codificador solo escribe `iCCP`), para que los colores no cambien.
fn png_color(bytes: &[u8], gray: bool) -> (Option<Vec<u8>>, ColorSpaceInfo) {
    let Some(png) = meta::png_meta(bytes) else {
        return (None, ColorSpaceInfo::AssumedSrgb);
    };
    if let Some((primaries, transfer)) = png.cicp {
        let (icc, color) = icc::from_cicp(primaries, transfer);
        return match icc {
            // Un ICC sintetizado desde cICP es RGB: no vale para píxeles en gris.
            Some(_) if gray => (
                None,
                ColorSpaceInfo::Discarded {
                    reason: "cICP en un PNG gris: se asume sRGB".to_owned(),
                },
            ),
            icc => (icc, color),
        };
    }
    if png.srgb {
        return (None, ColorSpaceInfo::AssumedSrgb);
    }
    icc::from_png_chunks(png.gamma, png.chromaticities, gray)
}

/// Lee el ICC de un TIFF directamente con la crate `tiff`.
///
/// `image` 0.25.10 pierde el ICC de TIFF pequeños cuando aplica límites de memoria (lo hace
/// siempre al decodificar con `ImageReader`); con esto no dependemos de ese fallo.
pub(crate) fn tiff_icc(bytes: &[u8]) -> Option<Vec<u8>> {
    meta::tiff_decoder(bytes)?
        .get_tag_u8_vec(tiff::tags::Tag::IccProfile)
        .ok()
}

pub(crate) fn is_gray(image: &DynamicImage) -> bool {
    image.color().channel_count() <= 2
}

fn bits_per_channel(color: ExtendedColorType) -> u8 {
    let channels = color.channel_count().max(1);
    (color.bits_per_pixel() / u16::from(channels)) as u8
}
