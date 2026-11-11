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
    // `image` no reconoce la firma de BigTIFF, aunque su decodificador TIFF sí lo lee.
    if reader.format().is_none() && (bytes.starts_with(b"II+\0") || bytes.starts_with(b"MM\0+")) {
        reader.set_format(ImageFormat::Tiff);
    }
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

    let mut decoder = reader
        .into_decoder()
        .map_err(|e| explain_tiff_error(bytes, format, e))?;
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
            } else if format == SourceFormat::Png {
                let (icc, color, note) = png_color(bytes, icc, gray);
                notes.extend(note);
                (icc, color)
            } else {
                icc::resolve(icc, gray)
            };
            (image, icc, color)
        }
    };

    match format {
        SourceFormat::Jpeg => {
            if let Some(j) = &jpeg {
                dpi = dpi.or(j.xmp_dpi).or(j.jfif_dpi);
                if !j.complete {
                    notes.push(
                        "El JPEG no termina como debe (¿archivo cortado?): la parte que falta \
                         sale en gris o en un color plano"
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

/// Espacio de color de un PNG. Como dicen la especificación PNG (3.ª edición) y hace macOS,
/// `cICP` manda sobre `iCCP`, y este sobre `sRGB` y `gAMA`/`cHRM`. `cICP` y `gAMA`/`cHRM` se
/// convierten en un ICC equivalente (nuestro codificador solo escribe `iCCP`), para que los
/// colores no cambien.
fn png_color(
    bytes: &[u8],
    icc: Option<Vec<u8>>,
    gray: bool,
) -> (Option<Vec<u8>>, ColorSpaceInfo, Option<String>) {
    let Some(png) = meta::png_meta(bytes) else {
        let (icc, color) = icc::resolve(icc, gray);
        return (icc, color, None);
    };
    // Un cICP con valores «sin especificar» (2) no dice nada: cuenta lo demás.
    if let Some((primaries, transfer)) = png.cicp.filter(|&(p, t)| p != 2 && t != 2) {
        match icc::from_cicp(primaries, transfer) {
            // Un ICC sintetizado desde cICP es RGB: no vale para píxeles en gris.
            (Some(_), _) if gray => {
                if icc.is_none() {
                    let reason = "cICP en un PNG gris: se asume sRGB".to_owned();
                    return (None, ColorSpaceInfo::Discarded { reason }, None);
                }
            }
            (cicp_icc, color @ ColorSpaceInfo::Nclx { .. }) => {
                let note = icc.is_some().then(|| {
                    "El PNG trae cICP y perfil ICC: manda cICP, como en macOS y en la \
                     especificación PNG"
                        .to_owned()
                });
                return (cicp_icc, color, note);
            }
            // cICP que no se puede convertir: se usa lo demás.
            _ => {}
        }
    }
    let (icc, color) = if icc.is_some() {
        icc::resolve(icc, gray)
    } else if png.srgb {
        (None, ColorSpaceInfo::AssumedSrgb)
    } else {
        icc::from_png_chunks(png.gamma, png.chromaticities, gray)
    };
    (icc, color, None)
}

/// Los TIFF en YCbCr (con submuestreo o con JPEG dentro) o en Lab no los lee `image`, que solo
/// dice «color type Unknown»: se explica qué pasa.
fn explain_tiff_error(bytes: &[u8], format: SourceFormat, e: image::ImageError) -> Error {
    if format != SourceFormat::Tiff || !matches!(e, image::ImageError::Unsupported(_)) {
        return e.into();
    }
    let photometric = meta::tiff_decoder(bytes).and_then(|mut d| {
        d.get_tag_u32(tiff::tags::Tag::PhotometricInterpretation)
            .ok()
    });
    let what = match photometric {
        Some(6) => "TIFF en YCbCr (con submuestreo o con JPEG dentro)",
        Some(8..=10) => "TIFF en color Lab",
        _ => return e.into(),
    };
    Error::Unsupported(format!(
        "{what}: Renuevo no lo lee. Ábrelo en Vista Previa y expórtalo como TIFF o PNG en RGB"
    ))
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
