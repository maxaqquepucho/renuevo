//! Ampliar resolución con algoritmos clásicos.

pub mod easu;
pub mod lanczos;
pub mod rcas;

use crate::planar::Planar;
use crate::{Progress, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UpscaleMethod {
    /// Lanczos3: el estándar de calidad, rápido.
    #[default]
    Lanczos3,
    /// EASU (FSR 1): sigue la dirección de los bordes, menos dientes de sierra.
    EdgeAdaptive,
}

impl UpscaleMethod {
    pub const ALL: [UpscaleMethod; 2] = [Self::Lanczos3, Self::EdgeAdaptive];

    pub fn label(self) -> &'static str {
        match self {
            Self::Lanczos3 => "Lanczos3 (natural)",
            Self::EdgeAdaptive => "Bordes nítidos (EASU)",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct UpscaleParams {
    /// Factor de ampliación (p. ej. 2.0 = el doble de ancho y de alto).
    pub factor: f32,
    pub method: UpscaleMethod,
    /// Enfoque RCAS posterior, 0..1 (0 = desactivado).
    pub sharpen: f32,
}

impl Default for UpscaleParams {
    fn default() -> Self {
        Self {
            factor: 2.0,
            method: UpscaleMethod::default(),
            sharpen: 0.4,
        }
    }
}

pub fn output_size(w: usize, h: usize, factor: f32) -> (usize, usize) {
    let f = f64::from(factor.max(0.01));
    (
        ((w as f64 * f).round() as usize).max(1),
        ((h as f64 * f).round() as usize).max(1),
    )
}

pub fn upscale(src: &Planar, params: &UpscaleParams, progress: &Progress) -> Result<Planar> {
    let (w, h) = (src.width, src.height);
    let (nw, nh) = output_size(w, h, params.factor);

    let mut out = match params.method {
        UpscaleMethod::EdgeAdaptive if params.factor > 1.0 => {
            // EASU está diseñado para factores ≤ 2: encadenamos pasadas iguales.
            let steps = params.factor.log2().ceil().max(1.0) as usize;
            let step_factor = params.factor.powf(1.0 / steps as f32);
            let mut current = src.clone();
            current.alpha = None;
            for step in 1..=steps {
                let (sw, sh) = if step == steps {
                    (nw, nh)
                } else {
                    output_size(w, h, step_factor.powi(step as i32))
                };
                progress.begin(format!("Ampliando (bordes) {step}/{steps}"), sh as u64);
                current = easu::easu_pass(&current, sw, sh, 1.0 / step_factor, progress)?;
            }
            current
        }
        _ => {
            progress.begin("Ampliando (Lanczos3)", 3);
            let mut out = Planar::new(nw, nh, src.gray, false);
            for (dst, plane) in out.channels.iter_mut().zip(&src.channels) {
                progress.check()?;
                *dst = lanczos::resize_plane(plane, w, h, nw, nh)?;
                progress.advance(1);
            }
            out
        }
    };

    if let Some(alpha) = &src.alpha {
        out.alpha = Some(lanczos::resize_plane(alpha, w, h, nw, nh)?);
    }

    if params.sharpen > 0.0 {
        progress.begin("Enfocando (RCAS)", 1);
        progress.check()?;
        out = rcas::rcas(&out, 2.0 * (1.0 - params.sharpen.min(1.0)));
        progress.advance(1);
    }
    Ok(out)
}
