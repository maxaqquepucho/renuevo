//! Errores del núcleo. Los mensajes están en español porque se muestran tal cual en la app.

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("no se pudo leer o escribir el archivo: {0}")]
    Io(#[from] std::io::Error),

    #[error("{0}")]
    Image(#[from] image::ImageError),

    #[cfg(feature = "heic")]
    #[error("no se pudo leer el HEIC: {0}")]
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
