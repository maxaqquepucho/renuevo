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

/// Factores de cada pasada de EASU, pensado para factores ≤ 2: una primera pasada de
/// ×(factor/2ⁿ) y n pasadas de ×2 (×3 = ×1,5 y ×2; ×4 = ×2 y ×2). Así cada tamaño intermedio
/// cae en una rejilla racional y una zona recortada se amplía exactamente igual que la foto
/// completa (con ×√3 y ×√3 no había forma de alinearla).
fn easu_factors(factor: f32) -> Vec<f32> {
    let doublings = (factor.log2().ceil() as i32 - 1).max(0);
    let first = factor / 2f32.powi(doublings);
    std::iter::once(first)
        .chain(std::iter::repeat_n(2.0, doublings as usize))
        .collect()
}

/// Factor acumulado tras cada pasada (el último es el factor pedido).
fn cumulative_factors(params: &UpscaleParams) -> Vec<f32> {
    match params.method {
        UpscaleMethod::EdgeAdaptive if params.factor > 1.0 => easu_factors(params.factor)
            .iter()
            .scan(1.0f32, |acc, f| {
                *acc *= f;
                Some(*acc)
            })
            .collect(),
        _ => vec![params.factor],
    }
}

/// Tamaño de salida de cada pasada (una sola con Lanczos; la última, el tamaño final).
pub(crate) fn pass_sizes(w: usize, h: usize, params: &UpscaleParams) -> Vec<(usize, usize)> {
    let factors = cumulative_factors(params);
    let last = factors.len() - 1;
    factors
        .iter()
        .enumerate()
        .map(|(i, f)| output_size(w, h, if i == last { params.factor } else { *f }))
        .collect()
}

/// Menor `q` (≤ 8) tal que una zona que empiece en un múltiplo de `q` cae exactamente en la
/// rejilla de salida de cada pasada (`q`·factor acumulado entero); 1 si no hay ninguno.
pub fn grid_step(params: &UpscaleParams) -> u32 {
    let factors = cumulative_factors(params);
    (1..=8u32)
        .find(|q| {
            factors.iter().all(|f| {
                let v = f64::from(*f) * f64::from(*q);
                (v - v.round()).abs() < 1e-4
            })
        })
        .unwrap_or(1)
}

/// Amplía `src`. Con alfa se amplía el color premultiplicado (ver [`Planar::premultiply`]) y el
/// alfa con exactamente el mismo filtro, así el borde no se oscurece ni depende del color
/// oculto bajo α = 0; el resultado sale premultiplicado y `to_image` lo deshace.
pub fn upscale(src: &Planar, params: &UpscaleParams, progress: &Progress) -> Result<Planar> {
    upscale_at(src, params, (0, 0), progress)
}

/// Como [`upscale`], para un recorte que empieza en `origin` de la foto (múltiplo de
/// [`grid_step`]): EASU calcula las posiciones como en la foto completa.
pub(crate) fn upscale_at(
    src: &Planar,
    params: &UpscaleParams,
    origin: (usize, usize),
    progress: &Progress,
) -> Result<Planar> {
    if src.alpha.is_some() && !src.premultiplied {
        let mut premultiplied = src.clone();
        premultiplied.premultiply();
        return upscale_at(&premultiplied, params, origin, progress);
    }
    // Posición del recorte tras ampliarlo `f` veces en total (entera gracias a `grid_step`).
    let at = |f: f32| {
        let s = |v: usize| (v as f64 * f64::from(f)).round() as usize;
        (s(origin.0), s(origin.1))
    };
    let (w, h) = (src.width, src.height);
    let (nw, nh) = output_size(w, h, params.factor);

    let mut out = match params.method {
        UpscaleMethod::EdgeAdaptive if params.factor > 1.0 => {
            let factors = easu_factors(params.factor);
            let sizes = pass_sizes(w, h, params);
            let steps = factors.len();
            let mut current: Option<Planar> = None;
            let mut done = 1.0f32;
            for (step, (f, (sw, sh))) in factors.into_iter().zip(sizes).enumerate() {
                let pass_origin = (at(done), at(done * f));
                done *= f;
                progress.begin(
                    format!("Ampliando (bordes) {}/{steps}", step + 1),
                    sh as u64,
                );
                // La primera pasada lee `src` directamente, sin copiarlo.
                let input = current.as_ref().unwrap_or(src);
                let next = easu::easu_pass(input, sw, sh, 1.0 / f, pass_origin, progress)?;
                current = Some(next);
            }
            current.expect("al menos una pasada")
        }
        _ => {
            let planes = src.channels.len() + usize::from(src.alpha.is_some());
            progress.begin("Ampliando (Lanczos3)", planes as u64);
            let mut resized = Vec::with_capacity(planes);
            for plane in src.channels.iter().chain(src.alpha.as_ref()) {
                progress.check()?;
                resized.push(lanczos::resize_plane(plane, w, h, nw, nh)?);
                progress.advance(1);
            }
            let alpha = src.alpha.as_ref().and_then(|_| resized.pop());
            Planar {
                width: nw,
                height: nh,
                channels: resized,
                alpha,
                premultiplied: src.premultiplied,
            }
        }
    };

    if params.sharpen > 0.0 {
        progress.begin("Enfocando (RCAS)", 1);
        progress.check()?;
        rcas::rcas(&mut out, 2.0 * (1.0 - params.sharpen.min(1.0)));
        progress.advance(1);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn easu_steps_are_rational() {
        assert_eq!(easu_factors(1.5), [1.5]);
        assert_eq!(easu_factors(2.0), [2.0]);
        assert_eq!(easu_factors(3.0), [1.5, 2.0]);
        assert_eq!(easu_factors(4.0), [2.0, 2.0]);
        assert_eq!(easu_factors(16.0), [2.0; 4]);
        let easu = |factor| UpscaleParams {
            factor,
            method: UpscaleMethod::EdgeAdaptive,
            sharpen: 0.0,
        };
        // ×3 = ×1,5 y ×2: la zona debe empezar en columna par.
        assert_eq!(grid_step(&easu(3.0)), 2);
        assert_eq!(grid_step(&easu(1.5)), 2);
        assert_eq!(grid_step(&easu(4.0)), 1);
    }
}
