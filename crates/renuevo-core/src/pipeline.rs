//! Orquesta el procesado: Restaurar → Ampliar (→ RCAS) → convertir a 16/8 bits.
//! Sin etapas activas es una copia exacta: los píxeles no se tocan.

use std::time::{Duration, Instant};

use image::DynamicImage;

use crate::io::OutputFormat;
use crate::photo::Dpi;
use crate::planar::{BitDepth, Planar};
use crate::restore::{NoiseLevels, RestoreParams, RestoreReport, restore};
use crate::upscale::{UpscaleParams, output_size, upscale};
use crate::{Error, Progress, Result};

/// Límite por defecto del tamaño de salida (megapíxeles).
pub const DEFAULT_MAX_OUTPUT_MEGAPIXELS: f64 = 250.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PipelineConfig {
    pub restore: Option<RestoreParams>,
    pub upscale: Option<UpscaleParams>,
    pub output_depth: BitDepth,
    pub max_output_megapixels: f64,
    /// Ruido de la foto completa (ver [`crate::restore::estimate_noise`]). Si falta, se mide
    /// sobre la imagen que se procese; para la vista previa de una zona conviene pasarlo.
    pub noise: Option<NoiseLevels>,
}

/// Factor máximo de ampliación aceptado.
pub const MAX_UPSCALE_FACTOR: f32 = 16.0;

impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            restore: None,
            upscale: None,
            output_depth: BitDepth::Bits16,
            max_output_megapixels: DEFAULT_MAX_OUTPUT_MEGAPIXELS,
            noise: None,
        }
    }
}

impl PipelineConfig {
    pub fn exact_copy() -> Self {
        Self::default()
    }

    pub fn is_exact_copy(&self) -> bool {
        self.restore.is_none_or(|r| r.is_noop()) && self.upscale.is_none()
    }

    /// WebP solo admite 8 bits: si es el destino, el procesado sale en 8 bits.
    pub fn for_format(mut self, format: OutputFormat) -> Self {
        if format == OutputFormat::WebP {
            self.output_depth = BitDepth::Bits8;
        }
        self
    }

    /// DPI de la salida: al ampliar se conserva el tamaño de impresión (más puntos por pulgada).
    pub fn output_dpi(&self, dpi: Option<Dpi>) -> Option<Dpi> {
        match self.upscale {
            Some(u) => dpi?.scaled(f64::from(u.factor)),
            None => dpi,
        }
    }

    pub fn output_size(&self, w: u32, h: u32) -> (u32, u32) {
        match self.upscale {
            Some(u) => {
                let (nw, nh) = output_size(w as usize, h as usize, u.factor);
                (nw as u32, nh as u32)
            }
            None => (w, h),
        }
    }

    /// Memoria aproximada (bytes) en el pico del procesado. 0 para la copia exacta.
    pub fn estimate_peak_bytes(&self, w: u32, h: u32) -> u64 {
        if self.is_exact_copy() {
            return 0;
        }
        let in_px = u64::from(w) * u64::from(h);
        let (nw, nh) = self.output_size(w, h);
        let out_px = u64::from(nw) * u64::from(nh);
        let planes = 12; // 3 canales f32
        let restore = if self.restore.is_some() {
            in_px * 4 * 8
        } else {
            0
        };
        let upscale = if self.upscale.is_some() {
            out_px * planes * 2
        } else {
            0
        };
        in_px * planes + restore + upscale + out_px * 8
    }

    fn check_size(&self, w: u32, h: u32) -> Result<()> {
        let factor = self.upscale.map_or(1.0, |u| u.factor);
        if !(factor.is_finite() && factor > 0.0 && factor <= MAX_UPSCALE_FACTOR) {
            return Err(Error::Unsupported(format!(
                "factor de ampliación no válido: {factor} (debe estar entre 0 y {MAX_UPSCALE_FACTOR})"
            )));
        }
        // En f64 y antes de convertir a u32, para que nada se trunque.
        let (nw, nh) = (
            (f64::from(w) * f64::from(factor)).round(),
            (f64::from(h) * f64::from(factor)).round(),
        );
        let mp = nw * nh / 1e6;
        if mp > self.max_output_megapixels {
            return Err(Error::TooLarge(format!(
                "{nw}×{nh} = {mp:.0} MP supera el límite de {:.0} MP (necesitaría ~{:.1} GB de RAM)",
                self.max_output_megapixels,
                self.estimate_peak_bytes(w, h) as f64 / 1e9
            )));
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct Processed {
    pub image: DynamicImage,
    pub restore_report: Option<RestoreReport>,
    pub elapsed: Duration,
}

pub fn process(
    image: &DynamicImage,
    cfg: &PipelineConfig,
    progress: &Progress,
) -> Result<Processed> {
    let start = Instant::now();
    if cfg.is_exact_copy() {
        return Ok(Processed {
            image: image.clone(),
            restore_report: None,
            elapsed: start.elapsed(),
        });
    }
    cfg.check_size(image.width(), image.height())?;

    progress.begin("Preparando", 1);
    let mut planar = Planar::from_image(image);
    progress.check()?;

    let restore_report = match &cfg.restore {
        Some(params) if !params.is_noop() => {
            Some(restore(&mut planar, params, cfg.noise, progress)?)
        }
        _ => None,
    };
    if let Some(params) = &cfg.upscale {
        planar = upscale(&planar, params, progress)?;
    }

    progress.begin("Generando imagen final", 1);
    progress.check()?;
    let image = planar.to_image(cfg.output_depth);
    progress.advance(1);
    Ok(Processed {
        image,
        restore_report,
        elapsed: start.elapsed(),
    })
}

/// Procesa solo una región (para la vista previa rápida). Añade un margen alrededor para que
/// los filtros no vean bordes artificiales y luego lo recorta. Devuelve la región ya escalada.
pub fn process_region(
    image: &DynamicImage,
    cfg: &PipelineConfig,
    region: (u32, u32, u32, u32),
    progress: &Progress,
) -> Result<DynamicImage> {
    const MARGIN: u32 = 24;
    let (x, y, w, h) = region;
    let (iw, ih) = (image.width(), image.height());
    let (x, y) = (x.min(iw.saturating_sub(1)), y.min(ih.saturating_sub(1)));
    let (w, h) = (w.min(iw - x).max(1), h.min(ih - y).max(1));
    // El origen del recorte se alinea a la rejilla del factor (p. ej. par para ×1,5) para que
    // los píxeles de la zona caigan exactamente donde caerían en la foto completa.
    let grid = cfg.upscale.map_or(1, |u| grid_step(u.factor));
    let snap = |v: u32| v.saturating_sub(MARGIN) / grid * grid;
    let (x0, y0) = (snap(x), snap(y));
    let (x1, y1) = ((x + w + MARGIN).min(iw), (y + h + MARGIN).min(ih));
    let crop = image.crop_imm(x0, y0, x1 - x0, y1 - y0);

    let processed = process(&crop, cfg, progress)?.image;
    let factor = cfg.upscale.map_or(1.0, |u| f64::from(u.factor));
    let scale = |v: u32| (f64::from(v) * factor).round() as u32;
    let (ox, oy) = (scale(x - x0), scale(y - y0));
    let ow = scale(w).min(processed.width().saturating_sub(ox)).max(1);
    let oh = scale(h).min(processed.height().saturating_sub(oy)).max(1);
    Ok(processed.crop_imm(ox, oy, ow, oh))
}

/// Menor `q` (≤ 8) tal que `factor·q` es entero; 1 si no hay ninguno.
fn grid_step(factor: f32) -> u32 {
    (1..=8u32)
        .find(|q| {
            let v = f64::from(factor) * f64::from(*q);
            (v - v.round()).abs() < 1e-4
        })
        .unwrap_or(1)
}
