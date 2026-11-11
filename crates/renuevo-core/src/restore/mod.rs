//! Restaurar / limpiar fotos con algoritmos clásicos.
//!
//! Orden: deblocking JPEG → ruido de color (filtro guiado) → ruido de luminancia (NLM) → enfoque.
//! Todo se hace en YCbCr: el ruido de color y el de luminancia se tratan por separado.

pub mod chroma;
pub mod deblock;
pub mod nlm;
pub mod noise;
pub mod sharpen;

use crate::color::{rgb_to_ycbcr, ycbcr_to_rgb};
use crate::planar::Planar;
use crate::{Progress, Result};

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

/// Ruido medido en una foto (escala 0..1). Se mide una vez sobre la foto completa y se reutiliza,
/// para que la vista previa de una zona y la exportación usen exactamente los mismos parámetros.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct NoiseLevels {
    pub luma: f32,
    pub chroma: f32,
}

/// Mide el ruido de luminancia y de color de una foto.
pub fn estimate_noise(image: &image::DynamicImage) -> NoiseLevels {
    let mut p = Planar::from_image(image);
    rgb_to_ycbcr(&mut p);
    noise_of_ycbcr(&p)
}

fn noise_of_ycbcr(p: &Planar) -> NoiseLevels {
    let (w, h) = (p.width, p.height);
    // JPEG/HEIC submuestrean el color: sus manchas abarcan bloques de 16×16.
    NoiseLevels {
        luma: noise::estimate_sigma(&p.channels[0], w, h),
        chroma: noise::estimate_sigma_blocks(&p.channels[1], w, h, 16)
            .max(noise::estimate_sigma_blocks(&p.channels[2], w, h, 16)),
    }
}

/// Restaura `p`. Si `noise` es `None`, el ruido se mide sobre `p`.
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
    rgb_to_ycbcr(p);
    // El ruido se mide sobre la foto tal como llega, antes de tocarla.
    let levels = noise.unwrap_or_else(|| noise_of_ycbcr(p));
    let (sigma, chroma_sigma) = (levels.luma, levels.chroma);
    report.noise_sigma_8bit = sigma * 255.0;
    report.chroma_sigma_8bit = chroma_sigma * 255.0;

    if params.deblock > 0.0 {
        progress.begin("Reduciendo bloques JPEG", 3);
        for plane in p.channels.iter_mut() {
            progress.check()?;
            report.blocking_detected |= deblock::deblock(plane, w, h, params.deblock);
            progress.advance(1);
        }
    }

    let [y, cb, cr] = &mut p.channels;

    if params.chroma_noise > 0.0 && !p.gray {
        progress.begin("Reduciendo ruido de color", 2);
        chroma::denoise_chroma(cb, w, h, params.chroma_noise, chroma_sigma);
        progress.advance(1);
        progress.check()?;
        chroma::denoise_chroma(cr, w, h, params.chroma_noise, chroma_sigma);
        progress.advance(1);
    }

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

    ycbcr_to_rgb(p);
    Ok(report)
}
