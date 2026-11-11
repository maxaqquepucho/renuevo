//! Errores del núcleo. Los mensajes están en español porque se muestran tal cual en la app; el
//! detalle técnico de las librerías (en inglés) se añade al final entre paréntesis.

use image::error::{ImageFormatHint, UnsupportedErrorKind};
use image::{ImageError, ImageFormat};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{}", io_message(.0))]
    Io(#[from] std::io::Error),

    #[error("{}", image_message(.0))]
    Image(#[from] ImageError),

    #[cfg(feature = "heic")]
    #[error("{}", heif_message(.0))]
    Heic(#[from] libheif_rs::HeifError),

    #[error("formato no soportado: {0}")]
    Unsupported(String),

    #[error("operación cancelada")]
    Cancelled,

    #[error("la imagen resultante sería demasiado grande: {0}")]
    TooLarge(String),

    #[error("la verificación pixel a pixel falló: {0}")]
    VerificationFailed(String),

    #[error("error al redimensionar: {0}")]
    Resize(String),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

const DAMAGED: &str = "está dañado o incompleto (¿archivo cortado?)";

fn io_message(e: &std::io::Error) -> String {
    match e.kind() {
        std::io::ErrorKind::UnexpectedEof => format!("El archivo {DAMAGED}"),
        // Los decodificadores devuelven así los datos comprimidos corruptos (p. ej. LZW de TIFF).
        std::io::ErrorKind::InvalidData => format!("El archivo {DAMAGED} ({e})"),
        _ => format!("no se pudo leer o escribir el archivo: {e}"),
    }
}

fn format_name(hint: &ImageFormatHint) -> String {
    match hint {
        ImageFormatHint::Exact(format) => match format {
            ImageFormat::Jpeg => "JPEG".to_owned(),
            ImageFormat::Png => "PNG".to_owned(),
            ImageFormat::WebP => "WebP".to_owned(),
            ImageFormat::Tiff => "TIFF".to_owned(),
            ImageFormat::Gif => "GIF".to_owned(),
            ImageFormat::Avif => "AVIF".to_owned(),
            other => format!("{other:?}").to_uppercase(),
        },
        ImageFormatHint::Name(name) => name.to_uppercase(),
        ImageFormatHint::PathExtension(ext) => ext.display().to_string().to_uppercase(),
        _ => "de imagen".to_owned(),
    }
}

fn image_message(e: &ImageError) -> String {
    match e {
        ImageError::Decoding(d) => {
            format!(
                "El archivo {} {DAMAGED} ({d})",
                format_name(&d.format_hint())
            )
        }
        ImageError::IoError(io) => io_message(io),
        ImageError::Unsupported(u) => match u.kind() {
            UnsupportedErrorKind::Format(hint) => format!(
                "El formato {} no es compatible: Renuevo abre JPEG, PNG, WebP, TIFF y HEIC",
                format_name(&hint)
            ),
            UnsupportedErrorKind::Color(c) | UnsupportedErrorKind::ColorLayout(c) => {
                format!("Tipo de píxel no compatible ({c:?})")
            }
            _ => format!("Función no compatible ({u})"),
        },
        ImageError::Limits(l) => format!("La imagen supera el límite de memoria ({l})"),
        ImageError::Encoding(enc) => format!("No se pudo guardar la imagen ({enc})"),
        ImageError::Parameter(p) => format!("Parámetro no válido ({p})"),
    }
}

#[cfg(feature = "heic")]
fn heif_message(e: &libheif_rs::HeifError) -> String {
    use libheif_rs::{HeifErrorCode as Code, HeifErrorSubCode as Sub};
    let what = match (e.code, e.sub_code) {
        (_, Sub::NoOrInvalidPrimaryItem | Sub::NoPitmBox) => {
            "El HEIF no tiene una imagen principal válida (¿es un vídeo?)".to_owned()
        }
        (_, Sub::UnsupportedCodec)
        | (Code::UnsupportedFeature | Code::DecoderPluginError | Code::PluginLoadingError, _) => {
            "El HEIF usa un códec o una función que libheif no admite (p. ej. VVC o multicapa)"
                .to_owned()
        }
        (Code::UnsupportedFileType, _) => "No es un HEIF compatible".to_owned(),
        (Code::MemoryAllocationError, _) => {
            "El HEIC necesita más memoria de la permitida".to_owned()
        }
        (Code::InvalidInput, _) => format!("El HEIC {DAMAGED}"),
        _ => "No se pudo leer el HEIC".to_owned(),
    };
    format!("{what} ({})", e.message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn library_errors_are_explained_in_spanish() {
        let eof = Error::from(std::io::Error::from(std::io::ErrorKind::UnexpectedEof));
        assert!(eof.to_string().contains("dañado o incompleto"), "{eof}");
        let gif = Error::from(ImageError::Unsupported(
            image::error::UnsupportedError::from_format_and_kind(
                ImageFormatHint::Exact(ImageFormat::Gif),
                UnsupportedErrorKind::Format(ImageFormatHint::Exact(ImageFormat::Gif)),
            ),
        ));
        assert!(
            gif.to_string()
                .starts_with("El formato GIF no es compatible"),
            "{gif}"
        );
    }
}
