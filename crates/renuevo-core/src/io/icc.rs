//! Utilidades de perfiles de color ICC (con `moxcms`).

use moxcms::{
    Chromaticity, CicpColorPrimaries, CicpProfile, ColorPrimaries, ColorProfile, DataColorSpace,
    LocalizableString, MatrixCoefficients, ProfileText, TransferCharacteristics, XyY,
    curve_from_gamma,
};

use crate::photo::ColorSpaceInfo;

/// Texto legible de un perfil ICC, si lo trae.
pub fn describe(icc: &[u8]) -> Option<String> {
    let profile = ColorProfile::new_from_slice(icc).ok()?;
    profile.description.as_ref().and_then(text_of)
}

fn text_of(text: &ProfileText) -> Option<String> {
    let s = match text {
        ProfileText::PlainString(s) => s.clone(),
        ProfileText::Localizable(items) => items.first()?.value.clone(),
        ProfileText::Description(d) => {
            if d.unicode_string.trim().is_empty() {
                d.ascii_string.clone()
            } else {
                d.unicode_string.clone()
            }
        }
    };
    let s = s.trim_matches(char::from(0)).trim().to_owned();
    (!s.is_empty()).then_some(s)
}

/// Comprueba que el perfil corresponde a los píxeles (RGB para color, gris para escala de grises).
/// Devuelve el motivo si no encaja: p. ej. un JPEG CMYK que `image` convierte a RGB trae un ICC CMYK.
pub fn check_compatible(icc: &[u8], is_gray: bool) -> Result<(), String> {
    let profile = ColorProfile::new_from_slice(icc).map_err(|e| format!("ICC ilegible: {e}"))?;
    match (profile.color_space, is_gray) {
        (DataColorSpace::Rgb, false) | (DataColorSpace::Gray, true) => Ok(()),
        (cs, _) => Err(format!("perfil {cs:?} no compatible con los píxeles")),
    }
}

/// Decide qué ICC acompaña a los píxeles decodificados y cómo describirlo.
pub fn resolve(icc: Option<Vec<u8>>, is_gray: bool) -> (Option<Vec<u8>>, ColorSpaceInfo) {
    match icc {
        None => (None, ColorSpaceInfo::AssumedSrgb),
        Some(icc) => match check_compatible(&icc, is_gray) {
            Ok(()) => {
                let description = describe(&icc);
                (Some(icc), ColorSpaceInfo::Icc { description })
            }
            Err(reason) => (None, ColorSpaceInfo::Discarded { reason }),
        },
    }
}

/// Convierte una descripción NCLX/CICP (típica de HEIC) en un ICC equivalente.
///
/// sRGB no necesita perfil (es lo que se asume sin ICC). Display P3 (iPhone) y el resto
/// se sintetizan con `moxcms` para que los colores no cambien al pasar a PNG/TIFF.
pub fn from_cicp(primaries: u8, transfer: u8) -> (Option<Vec<u8>>, ColorSpaceInfo) {
    let label = cicp_name(primaries, transfer);
    let incomplete = || {
        (
            None,
            ColorSpaceInfo::Discarded {
                reason: format!("NCLX incompleto ({label}): se asume sRGB"),
            },
        )
    };
    let (Ok(cp), Ok(tc)) = (
        CicpColorPrimaries::try_from(primaries),
        TransferCharacteristics::try_from(transfer),
    ) else {
        return incomplete();
    };
    let srgb_like_transfer = matches!(
        tc,
        TransferCharacteristics::Srgb | TransferCharacteristics::Unspecified
    );
    match cp {
        CicpColorPrimaries::Bt709 | CicpColorPrimaries::Unspecified if srgb_like_transfer => (
            None,
            ColorSpaceInfo::Nclx {
                description: "sRGB".to_owned(),
            },
        ),
        CicpColorPrimaries::Smpte432 if srgb_like_transfer => {
            let icc = ColorProfile::new_display_p3().encode().ok();
            (
                icc,
                ColorSpaceInfo::Nclx {
                    description: "Display P3".to_owned(),
                },
            )
        }
        _ => {
            // Los píxeles ya son RGB de rango completo (libheif convirtió desde YCbCr), así que
            // el perfil debe declarar matriz identidad y rango completo (H.273 / ICC.1:2022).
            let mut profile = ColorProfile::new_from_cicp(CicpProfile {
                color_primaries: cp,
                transfer_characteristics: tc,
                matrix_coefficients: MatrixCoefficients::Identity,
                full_range: true,
            });
            // Con primarias o transferencia sin especificar, moxcms devuelve un perfil vacío
            // (sin curvas ni primarias): mejor no incrustar nada que incrustar algo inválido.
            if profile.red_trc.is_none() || profile.cicp.is_none() {
                return incomplete();
            }
            // Sin descripción, las apps que ignoran la etiqueta `cicp` no saben qué es.
            profile.description = Some(text(&label));
            match profile.encode() {
                Ok(icc) => (Some(icc), ColorSpaceInfo::Nclx { description: label }),
                Err(_) => incomplete(),
            }
        }
    }
}

/// Nombre legible de una combinación CICP (H.273) habitual.
fn cicp_name(primaries: u8, transfer: u8) -> String {
    let p = match primaries {
        1 => "BT.709".to_owned(),
        9 => "BT.2020".to_owned(),
        11 => "DCI-P3".to_owned(),
        12 => "Display P3".to_owned(),
        n => format!("primarias {n}"),
    };
    let t = match transfer {
        1 | 6 | 14 | 15 => "gamma de vídeo".to_owned(),
        8 => "lineal".to_owned(),
        13 => "sRGB".to_owned(),
        16 => "PQ (HDR)".to_owned(),
        18 => "HLG (HDR)".to_owned(),
        n => format!("transferencia {n}"),
    };
    format!("{p}, {t}")
}

fn text(value: &str) -> ProfileText {
    ProfileText::Localizable(vec![LocalizableString::new(
        "en".to_owned(),
        "US".to_owned(),
        value.to_owned(),
    )])
}

/// Convierte los chunks `gAMA`/`cHRM` de un PNG sin ICC en un perfil equivalente.
///
/// `gamma` es el valor del chunk (gamma de codificación, 1/2,2 ≈ 0,4545 para sRGB) y
/// `chromaticities` el punto blanco y las primarias roja, verde y azul (x, y).
pub fn from_png_chunks(
    gamma: Option<f32>,
    chromaticities: Option<[(f32, f32); 4]>,
    gray: bool,
) -> (Option<Vec<u8>>, ColorSpaceInfo) {
    const SRGB: [(f32, f32); 4] = [(0.3127, 0.329), (0.64, 0.33), (0.3, 0.6), (0.15, 0.06)];
    // Como ColorSync: sin gAMA, el cHRM solo no cambia cómo se ve la imagen.
    let Some(gamma) = gamma else {
        return (None, ColorSpaceInfo::AssumedSrgb);
    };
    let gamma_is_srgb = (gamma - 1.0 / 2.2).abs() < 0.002;
    let chroma_is_srgb = gray
        || chromaticities.is_none_or(|c| {
            c.iter()
                .zip(SRGB)
                .all(|(a, b)| (a.0 - b.0).abs() < 0.005 && (a.1 - b.1).abs() < 0.005)
        });
    if gamma_is_srgb && chroma_is_srgb {
        return (None, ColorSpaceInfo::AssumedSrgb);
    }
    let invalid = |what: &str| {
        (
            None,
            ColorSpaceInfo::Discarded {
                reason: format!("{what} del PNG no válido: se asume sRGB"),
            },
        )
    };
    // Gamma de pantalla (la inversa de la del chunk), como curva de potencia pura: con primarias
    // que no son sRGB, gAMA 0,45455 es una potencia 2,2 (así lo muestra macOS), no la curva sRGB.
    if !(gamma.is_finite() && (0.01..=10.0).contains(&gamma)) {
        return invalid("gAMA");
    }
    let display_gamma = 1.0 / gamma;

    let mut parts = Vec::new();
    let mut profile = if gray {
        ColorProfile::new_gray_with_gamma(display_gamma)
    } else {
        let mut p = ColorProfile::new_srgb();
        p.cicp = None;
        if let Some(c) = chromaticities.filter(|_| !chroma_is_srgb) {
            // Primarias casi alineadas (triángulo sin área) no definen un espacio de color.
            let [_, r, g, b] = c;
            let area = ((g.0 - r.0) * (b.1 - r.1) - (b.0 - r.0) * (g.1 - r.1)).abs() / 2.0;
            if !c
                .iter()
                .all(|(x, y)| x.is_finite() && y.is_finite() && *y > 0.0)
                || area.is_nan()
                || area <= 0.01
            {
                return invalid("cHRM");
            }
            let chroma = |(x, y): (f32, f32)| Chromaticity { x, y };
            p.update_rgb_colorimetry(
                XyY {
                    x: f64::from(c[0].0),
                    y: f64::from(c[0].1),
                    yb: 1.0,
                },
                ColorPrimaries {
                    red: chroma(c[1]),
                    green: chroma(c[2]),
                    blue: chroma(c[3]),
                },
            );
            let colorants = [p.red_colorant, p.green_colorant, p.blue_colorant];
            if !colorants
                .iter()
                .all(|c| c.x.is_finite() && c.y.is_finite() && c.z.is_finite())
            {
                return invalid("cHRM");
            }
            parts.push("primarias de cHRM".to_owned());
        }
        let curve = curve_from_gamma(display_gamma);
        p.red_trc = Some(curve.clone());
        p.green_trc = Some(curve.clone());
        p.blue_trc = Some(curve);
        p
    };
    parts.insert(0, format!("gamma {display_gamma:.2}"));
    let label = format!("PNG con {}", parts.join(" y "));
    profile.description = Some(text(&label));
    match profile.encode() {
        Ok(icc) => (
            Some(icc),
            ColorSpaceInfo::Converted {
                description: format!("{label} (gAMA/cHRM convertidos a perfil ICC)"),
            },
        ),
        Err(_) => invalid("gAMA/cHRM"),
    }
}
