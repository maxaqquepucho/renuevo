//! Restaurar / limpiar fotos con algoritmos clásicos.
//!
//! Orden: deblocking JPEG → ruido de color (filtro guiado) → ruido de luminancia (NLM) → enfoque.
//! Todo se hace en YCbCr: el ruido de color y el de luminancia se tratan por separado.

pub mod chroma;
pub mod deblock;
pub mod nlm;
pub mod noise;
pub mod sharpen;

use rayon::prelude::*;

use crate::color::{rgb_to_ycbcr, ycbcr_to_rgb};
use crate::filters::box_blur;
use crate::planar::{Planar, finite, u8_to_f32, u16_to_f32};
use crate::{Progress, Result};
use deblock::BlockGrid;
use noise::{Interleaved, PlanarRows};

/// Intensidades en 0..1. Un 0 desactiva la etapa.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RestoreParams {
    /// Reducción de bloques JPEG (solo tiene efecto si se detecta la rejilla de 8×8).
    pub deblock: f32,
    /// Ruido de color (manchas de colores en zonas oscuras).
    pub chroma_noise: f32,
    /// Ruido de luminancia (grano).
    pub luma_noise: f32,
    /// Enfoque final.
    pub sharpen: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Preset {
    Soft,
    Medium,
    Strong,
}

impl Preset {
    pub const ALL: [Preset; 3] = [Self::Soft, Self::Medium, Self::Strong];

    pub fn label(self) -> &'static str {
        match self {
            Self::Soft => "Suave",
            Self::Medium => "Medio",
            Self::Strong => "Fuerte",
        }
    }
}

impl RestoreParams {
    pub fn preset(preset: Preset, source_is_jpeg: bool) -> Self {
        let deblock = |v: f32| if source_is_jpeg { v } else { 0.0 };
        match preset {
            // El enfoque es bajo a propósito: medido contra el original, enfocar siempre aleja
            // de él, y en fotos ruidosas devuelve parte del grano. Se puede subir a mano.
            Preset::Soft => Self {
                deblock: deblock(0.5),
                chroma_noise: 0.35,
                luma_noise: 0.3,
                sharpen: 0.1,
            },
            Preset::Medium => Self {
                deblock: deblock(0.8),
                chroma_noise: 0.6,
                luma_noise: 0.55,
                sharpen: 0.1,
            },
            Preset::Strong => Self {
                deblock: deblock(1.0),
                chroma_noise: 0.9,
                luma_noise: 0.85,
                sharpen: 0.05,
            },
        }
    }

    pub fn is_noop(&self) -> bool {
        self.deblock <= 0.0
            && self.chroma_noise <= 0.0
            && self.luma_noise <= 0.0
            && self.sharpen <= 0.0
    }
}

/// Lo que la restauración averiguó sobre la foto (se muestra en la app).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RestoreReport {
    /// σ del ruido de luminancia estimado, en niveles de 8 bits (0..255).
    pub noise_sigma_8bit: f32,
    /// σ del ruido de color estimado (el mayor de Cb y Cr), en niveles de 8 bits.
    pub chroma_sigma_8bit: f32,
    pub blocking_detected: bool,
}

/// El enfoque ignora el detalle por debajo de este múltiplo del ruido estimado.
const SHARPEN_CORING: f32 = 2.0;

/// Radio (píxeles) del relleno del color oculto bajo α = 0 a partir del visible de alrededor.
pub(crate) const FILL_RADIUS: usize = 8;

/// Ruido medido en una foto (escala 0..1). Se mide una vez sobre la foto completa y se reutiliza,
/// para que la vista previa de una zona y la exportación usen exactamente los mismos parámetros.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct NoiseLevels {
    pub luma: f32,
    pub chroma: f32,
    /// Rejilla de bloques JPEG de la foto completa. Si falta, se detecta sobre lo que se procese.
    pub grid: Option<BlockGrid>,
}

impl NoiseLevels {
    /// Los mismos niveles vistos desde un recorte que empieza en (`x0`, `y0`) de la foto.
    pub fn cropped(self, x0: usize, y0: usize) -> Self {
        Self {
            grid: self.grid.map(|g| g.cropped(x0, y0)),
            ..self
        }
    }
}

/// Mide el ruido de luminancia y de color de una foto, y su rejilla de bloques JPEG.
///
/// Lee el búfer de 8/16 bits por bloques de filas en paralelo, sin copiar la foto a `f32`: la
/// memoria extra es de unas pocas filas por hilo.
pub fn estimate_noise(image: &image::DynamicImage) -> NoiseLevels {
    use image::DynamicImage as D;
    fn rows<T: Copy + Sync>(
        raw: &[T],
        image: &D,
        channels: usize,
        has_alpha: bool,
        to_f32: fn(T) -> f32,
    ) -> NoiseLevels {
        noise::measure(&Interleaved {
            raw,
            width: image.width() as usize,
            height: image.height() as usize,
            channels,
            has_alpha,
            to_f32,
        })
    }
    match image {
        D::ImageLuma8(b) => rows(b.as_raw(), image, 1, false, u8_to_f32),
        D::ImageLumaA8(b) => rows(b.as_raw(), image, 2, true, u8_to_f32),
        D::ImageRgb8(b) => rows(b.as_raw(), image, 3, false, u8_to_f32),
        D::ImageRgba8(b) => rows(b.as_raw(), image, 4, true, u8_to_f32),
        D::ImageLuma16(b) => rows(b.as_raw(), image, 1, false, u16_to_f32),
        D::ImageLumaA16(b) => rows(b.as_raw(), image, 2, true, u16_to_f32),
        D::ImageRgb16(b) => rows(b.as_raw(), image, 3, false, u16_to_f32),
        D::ImageRgba16(b) => rows(b.as_raw(), image, 4, true, u16_to_f32),
        D::ImageRgb32F(b) => rows(b.as_raw(), image, 3, false, finite),
        D::ImageRgba32F(b) => rows(b.as_raw(), image, 4, true, finite),
        other => {
            let rgba = other.to_rgba32f();
            rows(rgba.as_raw(), image, 4, true, finite)
        }
    }
}

/// Restaura `p`. Si `noise` es `None`, el ruido se mide sobre `p`.
///
/// Con alfa, `p` queda premultiplicado (ver [`Planar::premultiply`]); `to_image` lo deshace.
pub fn restore(
    p: &mut Planar,
    params: &RestoreParams,
    noise: Option<NoiseLevels>,
    progress: &Progress,
) -> Result<RestoreReport> {
    let (w, h) = (p.width, p.height);
    let mut report = RestoreReport::default();
    if params.is_noop() || p.is_empty() {
        return Ok(report);
    }
    p.premultiply();
    rgb_to_ycbcr(p);
    // El ruido y la rejilla se miden sobre la foto tal como llega, antes de tocarla.
    let levels = noise.unwrap_or_else(|| noise::measure(&PlanarRows(p)));
    let grid = levels
        .grid
        .or_else(|| noise::measure(&PlanarRows(p)).grid)
        .unwrap_or_default();
    let (sigma, chroma_sigma) = (levels.luma, levels.chroma);
    report.noise_sigma_8bit = sigma * 255.0;
    report.chroma_sigma_8bit = chroma_sigma * 255.0;

    // Los filtros trabajan con el color sin premultiplicar y el oculto bajo α = 0 rellenado con
    // el visible de alrededor: ni ese color oculto ni el propio borde del alfa entran en el
    // resultado. Al final se vuelve a premultiplicar.
    unpremultiply_filled(p);

    if params.deblock > 0.0 {
        progress.begin("Reduciendo bloques JPEG", p.channels.len() as u64);
        for (plane, g) in p.channels.iter_mut().zip(grid.planes) {
            progress.check()?;
            report.blocking_detected |= deblock::deblock(plane, w, h, params.deblock, g);
            progress.advance(1);
        }
    }

    if let [_, cb, cr] = &mut p.channels[..]
        && params.chroma_noise > 0.0
    {
        progress.begin("Reduciendo ruido de color", 2);
        chroma::denoise_chroma(cb, w, h, params.chroma_noise, chroma_sigma);
        progress.advance(1);
        progress.check()?;
        chroma::denoise_chroma(cr, w, h, params.chroma_noise, chroma_sigma);
        progress.advance(1);
    }

    let y = &mut p.channels[0];

    if params.luma_noise > 0.0 && sigma > 0.0 {
        // Barrido sobre fotos reales degradadas: el óptimo está en h ≈ 1,4·σ.
        let h_param = (0.5 + params.luma_noise) * sigma;
        let nlm_params = nlm::NlmParams {
            h: h_param,
            sigma: h_param,
            search_radius: 5,
            patch_radius: 2,
        };
        *y = nlm::nlm(y, w, h, nlm_params, progress)?;
    }

    if params.sharpen > 0.0 {
        progress.begin("Enfocando", 1);
        sharpen::unsharp(y, w, h, params.sharpen, 1.2, SHARPEN_CORING * sigma);
        progress.advance(1);
    }

    if let Some(alpha) = &p.alpha {
        for plane in &mut p.channels {
            plane
                .par_iter_mut()
                .zip(alpha.par_iter())
                .for_each(|(c, a)| *c *= a);
        }
    }
    ycbcr_to_rgb(p);
    Ok(report)
}

/// Deshace la premultiplicación de `p` (en cualquier espacio lineal, también YCbCr). Donde α = 0
/// pone la media del color visible en un radio de [`FILL_RADIUS`] ponderada por el alfa (0 si
/// no hay nada visible cerca).
fn unpremultiply_filled(p: &mut Planar) {
    let (w, h) = (p.width, p.height);
    let Some(alpha) = &p.alpha else { return };
    let weight = box_blur(alpha, w, h, FILL_RADIUS);
    for plane in &mut p.channels {
        let spread = box_blur(plane, w, h, FILL_RADIUS);
        plane
            .par_iter_mut()
            .zip(alpha.par_iter())
            .zip(spread.par_iter().zip(&weight))
            .for_each(|((c, a), (s, wt))| {
                *c = if *a > 0.0 {
                    *c / a
                } else if *wt > 1e-6 {
                    s / wt
                } else {
                    0.0
                };
            });
    }
}
