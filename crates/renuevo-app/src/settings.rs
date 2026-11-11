//! Ajustes elegidos en la interfaz y su traducción a la configuración del núcleo.

use renuevo_core::io::EncodeOptions;
use renuevo_core::pipeline::DEFAULT_MAX_OUTPUT_MEGAPIXELS;
use renuevo_core::restore::{Preset, RestoreParams};
use renuevo_core::upscale::UpscaleParams;
use renuevo_core::{BitDepth, PipelineConfig};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Regenerar la foto píxel a píxel sin cambiar nada (verificado).
    ExactCopy,
    /// Restaurar y/o ampliar.
    Enhance,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    pub mode: Mode,
    pub restore_enabled: bool,
    /// `None` = ajustes manuales.
    pub preset: Option<Preset>,
    pub restore: RestoreParams,
    pub upscale_enabled: bool,
    pub upscale: UpscaleParams,
    pub encode: EncodeOptions,
    pub output_depth: BitDepth,
    pub max_output_megapixels: f64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            mode: Mode::ExactCopy,
            restore_enabled: true,
            preset: Some(Preset::Medium),
            restore: RestoreParams::preset(Preset::Medium, false),
            upscale_enabled: false,
            upscale: UpscaleParams::default(),
            encode: EncodeOptions::default(),
            output_depth: BitDepth::Bits16,
            max_output_megapixels: DEFAULT_MAX_OUTPUT_MEGAPIXELS,
        }
    }
}

impl Settings {
    pub fn pipeline(&self) -> PipelineConfig {
        match self.mode {
            Mode::ExactCopy => PipelineConfig::exact_copy(),
            Mode::Enhance => PipelineConfig {
                restore: self.restore_enabled.then_some(self.restore),
                upscale: self.upscale_enabled.then_some(self.upscale),
                output_depth: self.output_depth,
                max_output_megapixels: self.max_output_megapixels,
                noise: None,
            }
            .for_format(self.encode.format),
        }
    }

    pub fn apply_preset(&mut self, preset: Preset, source_is_jpeg: bool) {
        self.preset = Some(preset);
        self.restore = RestoreParams::preset(preset, source_is_jpeg);
    }

    /// Al abrir otra foto, el deblocking del preset depende de si es JPEG.
    pub fn adapt_to_source(&mut self, source_is_jpeg: bool) {
        if let Some(preset) = self.preset {
            self.restore = RestoreParams::preset(preset, source_is_jpeg);
        }
    }
}
