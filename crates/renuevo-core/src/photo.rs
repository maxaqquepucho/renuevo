//! Representación de una foto decodificada junto con lo que sabemos de su origen.

use std::path::PathBuf;

use image::DynamicImage;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceFormat {
    Jpeg,
    Png,
    WebP,
    Tiff,
    /// HEIC/HEIF (fotos de iPhone) y otros contenedores HEIF.
    Heif,
    Other,
}

impl SourceFormat {
    pub fn label(self) -> &'static str {
        match self {
            Self::Jpeg => "JPEG",
            Self::Png => "PNG",
            Self::WebP => "WebP",
            Self::Tiff => "TIFF",
            Self::Heif => "HEIC/HEIF",
            Self::Other => "otro",
        }
    }

    /// Formatos con pérdida: la foto ya trae artefactos de compresión.
    pub fn is_lossy(self) -> bool {
        matches!(self, Self::Jpeg | Self::Heif)
    }
}

/// Espacio de color de la foto y de dónde salió.
#[derive(Debug, Clone, PartialEq)]
pub enum ColorSpaceInfo {
    /// El archivo no trae perfil: se asume sRGB.
    AssumedSrgb,
    /// Perfil ICC incrustado en el original (se conserva tal cual).
    Icc { description: Option<String> },
    /// HEIF con perfil NCLX/CICP: se sintetizó un ICC equivalente para no perder el color.
    Nclx { description: String },
    /// El color se convirtió con gestión de color a un perfil que PNG/TIFF/WebP sí pueden
    /// guardar (p. ej. CMYK con su perfil → Adobe RGB, o `gAMA`/`cHRM` de PNG → ICC).
    Converted { description: String },
    /// El original traía un perfil que no corresponde a los píxeles decodificados.
    Discarded { reason: String },
}

impl ColorSpaceInfo {
    pub fn describe(&self) -> String {
        match self {
            Self::AssumedSrgb => "sRGB (sin perfil)".to_owned(),
            Self::Icc { description } => match description {
                Some(d) => format!("ICC: {d}"),
                None => "ICC incrustado".to_owned(),
            },
            Self::Nclx { description } => format!("NCLX: {description}"),
            Self::Converted { description } => description.clone(),
            Self::Discarded { reason } => format!("perfil descartado ({reason})"),
        }
    }
}

/// Resolución de impresión (puntos por pulgada). No afecta a los píxeles.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Dpi {
    pub x: f64,
    pub y: f64,
}

impl Dpi {
    /// Solo valores con sentido (los archivos a veces traen 0 o cifras absurdas).
    pub fn new(x: f64, y: f64) -> Option<Dpi> {
        let ok = |v: f64| v.is_finite() && (1.0..=100_000.0).contains(&v);
        (ok(x) && ok(y)).then_some(Dpi { x, y })
    }

    /// Al ampliar ×`factor` se mantiene el tamaño de impresión: hay más puntos por pulgada.
    pub fn scaled(self, factor: f64) -> Option<Dpi> {
        Dpi::new(self.x * factor, self.y * factor)
    }
}

#[derive(Debug, Clone)]
pub struct SourceInfo {
    pub path: Option<PathBuf>,
    pub format: SourceFormat,
    pub file_size: u64,
    /// Bits por canal tal como venían en el archivo (8, 10, 12, 16…).
    pub bits_per_channel: u8,
    pub had_exif: bool,
    /// El EXIF original incluía coordenadas GPS (se eliminan al exportar).
    pub had_gps: bool,
    /// Los píxeles se giraron o voltearon para respetar la orientación del archivo.
    pub orientation_applied: bool,
    pub color: ColorSpaceInfo,
    /// Resolución de impresión del original, si la declaraba (se conserva en PNG y TIFF).
    pub dpi: Option<Dpi>,
    /// Avisos sobre lo que el original tenía y no se puede copiar (o se tuvo que convertir).
    pub notes: Vec<String>,
}

/// Una foto lista para procesar: píxeles ya orientados, perfil ICC y sin EXIF.
#[derive(Debug, Clone)]
pub struct Photo {
    pub image: DynamicImage,
    pub icc: Option<Vec<u8>>,
    pub info: SourceInfo,
}

impl Photo {
    pub fn width(&self) -> u32 {
        self.image.width()
    }

    pub fn height(&self) -> u32 {
        self.image.height()
    }

    pub fn pixel_count(&self) -> u64 {
        u64::from(self.width()) * u64::from(self.height())
    }

    pub fn megapixels(&self) -> f64 {
        self.pixel_count() as f64 / 1_000_000.0
    }

    /// Mismo origen y perfil, con píxeles nuevos (resultado de un procesado).
    pub fn with_image(&self, image: DynamicImage) -> Photo {
        Photo {
            image,
            icc: self.icc.clone(),
            info: self.info.clone(),
        }
    }
}
