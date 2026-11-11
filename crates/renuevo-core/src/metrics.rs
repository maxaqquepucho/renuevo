//! Métricas objetivas de calidad para los tests y la verificación: PSNR y SSIM.

use image::DynamicImage;

use crate::color::luma;
use crate::planar::Planar;

/// PSNR en dB entre dos imágenes del mismo tamaño (canales RGB en 0..1). `inf` si son iguales.
pub fn psnr(a: &DynamicImage, b: &DynamicImage) -> f64 {
    let (pa, pb) = (Planar::from_image(a), Planar::from_image(b));
    assert_eq!(
        (pa.width, pa.height),
        (pb.width, pb.height),
        "tamaños distintos"
    );
    let mut sum = 0.0f64;
    for (ca, cb) in pa.rgb().iter().zip(pb.rgb()) {
        sum += ca
            .iter()
            .zip(cb)
            .map(|(x, y)| f64::from(x - y).powi(2))
            .sum::<f64>();
    }
    let mse = sum / (3 * pa.len()) as f64;
    if mse == 0.0 {
        f64::INFINITY
    } else {
        10.0 * (1.0 / mse).log10()
    }
}

/// SSIM medio sobre la luminancia, con ventanas 8×8 desplazadas de 4 en 4.
pub fn ssim(a: &DynamicImage, b: &DynamicImage) -> f64 {
    let (pa, pb) = (Planar::from_image(a), Planar::from_image(b));
    assert_eq!(
        (pa.width, pa.height),
        (pb.width, pb.height),
        "tamaños distintos"
    );
    let (w, h) = (pa.width, pa.height);
    let la = luma_plane(&pa);
    let lb = luma_plane(&pb);
    const C1: f64 = 0.01 * 0.01;
    const C2: f64 = 0.03 * 0.03;
    const WIN: usize = 8;
    let (mut total, mut count) = (0.0f64, 0usize);
    let mut y = 0;
    while y + WIN <= h {
        let mut x = 0;
        while x + WIN <= w {
            let (mut sa, mut sb, mut saa, mut sbb, mut sab) = (0.0, 0.0, 0.0, 0.0, 0.0);
            for yy in y..y + WIN {
                for xx in x..x + WIN {
                    let (va, vb) = (f64::from(la[yy * w + xx]), f64::from(lb[yy * w + xx]));
                    sa += va;
                    sb += vb;
                    saa += va * va;
                    sbb += vb * vb;
                    sab += va * vb;
                }
            }
            let n = (WIN * WIN) as f64;
            let (ma, mb) = (sa / n, sb / n);
            let (va, vb) = (saa / n - ma * ma, sbb / n - mb * mb);
            let cov = sab / n - ma * mb;
            total += ((2.0 * ma * mb + C1) * (2.0 * cov + C2))
                / ((ma * ma + mb * mb + C1) * (va + vb + C2));
            count += 1;
            x += 4;
        }
        y += 4;
    }
    if count == 0 {
        1.0
    } else {
        total / count as f64
    }
}

fn luma_plane(p: &Planar) -> Vec<f32> {
    let [r, g, b] = p.rgb();
    r.iter()
        .zip(g)
        .zip(b)
        .map(|((r, g), b)| luma(*r, *g, *b))
        .collect()
}
