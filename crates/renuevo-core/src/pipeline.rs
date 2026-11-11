//! Orquesta el procesado: Restaurar → Ampliar (→ RCAS) → convertir a 16/8 bits.
//! Sin etapas activas es una copia exacta: los píxeles no se tocan.

use std::time::{Duration, Instant};

use image::{ColorType, DynamicImage};

use crate::io::OutputFormat;
use crate::photo::Dpi;
use crate::planar::{BitDepth, Planar};
use crate::restore::{
    FILL_RADIUS, NoiseLevels, RestoreParams, RestoreReport, estimate_noise, restore,
};
use crate::upscale::{
    UpscaleMethod, UpscaleParams, grid_step, output_size, pass_sizes, upscale_at,
};
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

    /// Memoria aproximada (bytes) que el procesado de una foto de `w`×`h` con los canales de
    /// `color` añade a la app en su pico (la foto ya abierta no cuenta). Sale algo por encima de
    /// lo medido, nunca por debajo. 0 para la copia exacta.
    pub fn estimate_peak_bytes_for(&self, w: u32, h: u32, color: ColorType) -> u64 {
        self.peak_bytes(w, h, color.channel_count() <= 2, color.has_alpha())
    }

    /// Como [`Self::estimate_peak_bytes_for`] sin saber cómo es la foto: supone el peor caso
    /// (color con alfa), así que con RGB o gris se queda más por encima.
    pub fn estimate_peak_bytes(&self, w: u32, h: u32) -> u64 {
        self.peak_bytes(w, h, false, true)
    }

    /// Suma de los búferes grandes que reserva el procesado. Cuenta también los que ya se han
    /// liberado cuando se reserva el siguiente: el asignador de macOS se los queda y casi nunca
    /// los reutiliza para un tamaño distinto, así que la memoria de la app sube eso. Solo los
    /// planos temporales de la restauración, todos del mismo tamaño, se reaprovechan entre
    /// etapas. Contrastado con el pico de memoria residente en fotos reales de 12 a 81 MP.
    fn peak_bytes(&self, w: u32, h: u32, gray: bool, alpha: bool) -> u64 {
        if self.is_exact_copy() {
            return 0;
        }
        let (w, h) = (u64::from(w), u64::from(h));
        let planes = if gray { 1 } else { 3 } + u64::from(alpha);
        // Un `Planar` de `px` píxeles: un plano f32 por canal.
        let planar = |px: u64| 4 * planes * px;
        let threads = rayon::current_num_threads() as u64;
        let in_px = w * h;
        let mut total = planar(in_px);
        if self.restore.is_some_and(|r| !r.is_noop()) {
            // Planos temporales: 4 en el filtro de color, 3 en el enfoque y en el relleno bajo
            // el alfa, 1 en el NLM; más las bandas del NLM de cada hilo (136 filas de f32).
            let temp = if gray { 3 } else { 4 };
            total += 4 * temp * in_px + threads * 4 * 136 * w;
        }
        let mut final_px = in_px;
        if let Some(u) = &self.upscale {
            let sizes = pass_sizes(w as usize, h as usize, u);
            let (nw, nh) = sizes.last().map_or((w, h), |&(a, b)| (a as u64, b as u64));
            final_px = nw * nh;
            if u.method == UpscaleMethod::EdgeAdaptive && u.factor > 1.0 {
                // Cada pasada: la luminancia de su entrada para analizar bordes (solo en
                // color) y su resultado.
                let mut input = in_px;
                for &(sw, sh) in &sizes {
                    let luma = if gray { 0 } else { 4 * input };
                    input = (sw * sh) as u64;
                    total += luma + planar(input);
                }
            } else {
                // Más el búfer intermedio de la pasada horizontal de `fast_image_resize`.
                total += 4 * (nw * h).max(w * nh) + planar(final_px);
            }
            if u.sharpen > 0.0 {
                // RCAS en el sitio: filas de borde de cada banda y tres filas por hilo.
                total += 4 * planes * nw * (2 * nh.div_ceil(128) + 3 * threads);
            }
        }
        // La imagen final.
        let depth = match self.output_depth {
            BitDepth::Bits16 => 2,
            BitDepth::Bits8 => 1,
        };
        total += planes * depth * final_px;
        // Margen para vectores pequeños y redondeos.
        total + total / 32 + (8 << 20)
    }

    fn check_size(&self, image: &DynamicImage) -> Result<()> {
        let (w, h) = (image.width(), image.height());
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
                self.estimate_peak_bytes_for(w, h, image.color()) as f64 / 1e9
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
    process_at(image, cfg, (0, 0), progress)
}

/// `origin`: posición de `image` en la foto completa (para la vista previa de una zona), así
/// la ampliación y el dither son exactamente los de la foto entera.
fn process_at(
    image: &DynamicImage,
    cfg: &PipelineConfig,
    origin: (usize, usize),
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
    cfg.check_size(image)?;

    progress.begin("Preparando", 1);
    let mut planar = Planar::from_image(image);
    // Con alfa se trabaja con el color premultiplicado: ver `Planar::premultiply`.
    planar.premultiply();
    progress.check()?;

    let restore_report = match &cfg.restore {
        Some(params) if !params.is_noop() => {
            Some(restore(&mut planar, params, cfg.noise, progress)?)
        }
        _ => None,
    };
    if let Some(params) = &cfg.upscale {
        planar = upscale_at(&planar, params, origin, progress)?;
    }

    progress.begin("Generando imagen final", 1);
    progress.check()?;
    // En 8 bits, dither: si no, el redondeo deja bandas en los degradados que suaviza el
    // procesado (cielos). La copia exacta no pasa por aquí.
    let factor = cfg.upscale.map_or(1.0, |u| f64::from(u.factor));
    let scaled = |v: usize| (v as f64 * factor).round() as usize;
    let image = match cfg.output_depth {
        BitDepth::Bits8 => {
            planar.to_image_dithered(BitDepth::Bits8, (scaled(origin.0), scaled(origin.1)))
        }
        BitDepth::Bits16 => planar.to_image(BitDepth::Bits16),
    };
    drop(planar);
    progress.advance(1);
    Ok(Processed {
        image,
        restore_report,
        elapsed: start.elapsed(),
    })
}

/// Procesa solo una región (para la vista previa rápida). Añade un margen alrededor para que
/// los filtros no vean bordes artificiales y luego lo recorta. Devuelve la región ya escalada,
/// igual que la misma zona del resultado de la foto completa.
pub fn process_region(
    image: &DynamicImage,
    cfg: &PipelineConfig,
    region: (u32, u32, u32, u32),
    progress: &Progress,
) -> Result<DynamicImage> {
    const MARGIN: u32 = 24;
    let restoring = cfg.restore.is_some_and(|r| !r.is_noop());
    // Con alfa, la restauración rellena el color oculto con el visible de alrededor: los filtros
    // llegan más lejos.
    let margin = if restoring && image.color().has_alpha() {
        MARGIN + FILL_RADIUS as u32
    } else {
        MARGIN
    };
    let (x, y, w, h) = region;
    let (iw, ih) = (image.width(), image.height());
    let (x, y) = (x.min(iw.saturating_sub(1)), y.min(ih.saturating_sub(1)));
    let (w, h) = (w.min(iw - x).max(1), h.min(ih - y).max(1));
    // El origen del recorte se alinea a la rejilla del factor (p. ej. par para ×1,5 o ×3) para
    // que los píxeles de la zona caigan exactamente donde caerían en la foto completa.
    let grid = cfg.upscale.map_or(1, |u| grid_step(&u));
    let snap = |v: u32| v.saturating_sub(margin) / grid * grid;
    let (x0, y0) = (snap(x), snap(y));
    let (x1, y1) = ((x + w + margin).min(iw), (y + h + margin).min(ih));
    let crop = image.crop_imm(x0, y0, x1 - x0, y1 - y0);

    // El ruido y la rejilla JPEG son los de la foto completa: el recorte solo ve una parte y
    // podría no detectar la rejilla que sí se trata en la foto entera.
    let mut cfg = *cfg;
    if restoring {
        let mut noise = cfg.noise.unwrap_or_else(|| estimate_noise(image));
        if noise.grid.is_none() {
            noise.grid = estimate_noise(image).grid;
        }
        cfg.noise = Some(noise.cropped(x0 as usize, y0 as usize));
    }

    let factor = cfg.upscale.map_or(1.0, |u| f64::from(u.factor));
    let scale = |v: u32| (f64::from(v) * factor).round() as u32;
    let processed = process_at(&crop, &cfg, (x0 as usize, y0 as usize), progress)?.image;
    let (ox, oy) = (scale(x - x0), scale(y - y0));
    let ow = scale(w).min(processed.width().saturating_sub(ox)).max(1);
    let oh = scale(h).min(processed.height().saturating_sub(oy)).max(1);
    Ok(processed.crop_imm(ox, oy, ow, oh))
}
