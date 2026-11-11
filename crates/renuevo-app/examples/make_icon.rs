//! Genera `crates/renuevo-app/assets/icon.png` (1024×1024), el icono de la app:
//! un paisaje al amanecer partido por el divisor antes/después del visor, pixelado a la
//! izquierda y nítido a la derecha.
//!
//! `cargo run -p renuevo-app --example make_icon`

use image::{ImageBuffer, Rgba};

const SIZE: u32 = 1024;
/// Cuerpo del icono según la plantilla de macOS: 824 px centrados (100 px de margen).
const BODY: f32 = 824.0;
const MARGIN: f32 = 100.0;
/// Exponente de la superelipse que aproxima la esquina continua de los iconos de macOS.
const SQUIRCLE: f32 = 5.0;
/// Bloques por lado en la mitad pixelada.
const BLOCKS: f32 = 16.0;
const SUPERSAMPLE: u32 = 4;

type Color = [f32; 3];

fn hex(rgb: u32) -> Color {
    let c = |shift: u32| ((rgb >> shift) & 0xff) as f32 / 255.0;
    [c(16), c(8), c(0)]
}

fn mix(a: Color, b: Color, t: f32) -> Color {
    [0, 1, 2].map(|i| a[i] + (b[i] - a[i]) * t)
}

fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Altura (0 arriba, 1 abajo) de una cordillera definida por sus picos, interpolando en línea
/// recta entre ellos.
fn ridge(peaks: &[(f32, f32)], u: f32) -> f32 {
    for pair in peaks.windows(2) {
        let ((u0, v0), (u1, v1)) = (pair[0], pair[1]);
        if u <= u1 {
            return v0 + (v1 - v0) * ((u - u0) / (u1 - u0)).clamp(0.0, 1.0);
        }
    }
    peaks.last().map_or(1.0, |p| p.1)
}

/// El paisaje en coordenadas del cuerpo (0..1 en cada eje).
fn landscape(u: f32, v: f32) -> Color {
    let sky_top = hex(0x2b2d6e);
    let sky_horizon = hex(0xf6a35c);
    let mut c = mix(sky_top, sky_horizon, (v / 0.66).clamp(0.0, 1.0).powf(1.4));

    // Sol con un halo suave.
    let (sx, sy, sr) = (0.5, 0.33, 0.13);
    let d = ((u - sx).powi(2) + (v - sy).powi(2)).sqrt();
    c = mix(c, hex(0xffd88a), 0.55 * (-(d - sr).max(0.0) / 0.07).exp());
    if d < sr {
        c = hex(0xffe9b0);
    }

    let back = [
        (0.0, 0.60),
        (0.20, 0.42),
        (0.38, 0.56),
        (0.56, 0.38),
        (0.78, 0.54),
        (1.0, 0.45),
    ];
    if v > ridge(&back, u) {
        c = hex(0x5b4b8a);
    }
    let front = [
        (0.0, 0.73),
        (0.28, 0.61),
        (0.52, 0.71),
        (0.76, 0.59),
        (1.0, 0.69),
    ];
    if v > ridge(&front, u) {
        c = mix(
            hex(0x1f5e6e),
            hex(0x163f4d),
            ((v - 0.6) / 0.4).clamp(0.0, 1.0),
        );
    }
    c
}

/// Color del cuerpo en (u, v): paisaje pixelado a la izquierda, nítido a la derecha, y el
/// divisor con su asa en el centro.
fn body(u: f32, v: f32) -> Color {
    let px = 1.0 / BODY;
    let (hx, hy, hr) = (0.5, 0.5, 0.072);
    let dh = ((u - hx).powi(2) + (v - hy).powi(2)).sqrt();
    if dh < hr {
        // Asa: blanca con dos flechas (◀ ▶) del color del cielo.
        let (ax, ay) = ((u - hx).abs(), (v - hy).abs());
        let arrow = ax > 0.012 && ax < 0.046 && ay < (0.046 - ax) * 0.95;
        return if arrow { hex(0x2b2d6e) } else { [1.0; 3] };
    }
    if (u - 0.5).abs() < 5.0 * px {
        return [1.0; 3];
    }
    if u < 0.5 {
        let cell = |t: f32| ((t * BLOCKS).floor() + 0.5) / BLOCKS;
        landscape(cell(u), cell(v))
    } else {
        landscape(u, v)
    }
}

/// «Distancia» aproximada en píxeles al borde de la superelipse del cuerpo desplazado `dy`
/// (negativa dentro).
fn squircle_distance(x: f32, y: f32, dy: f32) -> f32 {
    let half = BODY / 2.0;
    let nx = ((x - MARGIN - half) / half).abs();
    let ny = ((y - MARGIN - half - dy) / half).abs();
    ((nx.powf(SQUIRCLE) + ny.powf(SQUIRCLE)).powf(1.0 / SQUIRCLE) - 1.0) * half
}

fn main() {
    let srgb_to_u8 = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    let icon = ImageBuffer::from_fn(SIZE, SIZE, |x, y| {
        let n = SUPERSAMPLE * SUPERSAMPLE;
        let (mut rgb, mut cover) = ([0.0f32; 3], 0.0f32);
        for i in 0..n {
            let sx = x as f32 + ((i % SUPERSAMPLE) as f32 + 0.5) / SUPERSAMPLE as f32;
            let sy = y as f32 + ((i / SUPERSAMPLE) as f32 + 0.5) / SUPERSAMPLE as f32;
            if squircle_distance(sx, sy, 0.0) < 0.0 {
                let c = body((sx - MARGIN) / BODY, (sy - MARGIN) / BODY);
                (0..3).for_each(|k| rgb[k] += c[k]);
                cover += 1.0;
            }
        }
        let alpha = cover / n as f32;
        // Sombra suave bajo el cuerpo, como la de los iconos del sistema.
        let centre = (x as f32 + 0.5, y as f32 + 0.5);
        let shadow =
            0.30 * (1.0 - smoothstep(-6.0, 26.0, squircle_distance(centre.0, centre.1, 12.0)));
        let out_alpha = alpha + shadow * (1.0 - alpha);
        if out_alpha <= 0.0 {
            return Rgba([0, 0, 0, 0]);
        }
        // Color sin premultiplicar: el cuerpo sobre la sombra negra.
        let color = rgb.map(|c| if cover > 0.0 { c / cover } else { 0.0 });
        let k = alpha / out_alpha;
        Rgba([
            srgb_to_u8(color[0] * k),
            srgb_to_u8(color[1] * k),
            srgb_to_u8(color[2] * k),
            srgb_to_u8(out_alpha),
        ])
    });
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/icon.png");
    std::fs::create_dir_all(path.parent().expect("carpeta")).expect("crear assets/");
    icon.save(&path).expect("guardar el icono");
    println!("{}", path.display());
}
