//! Mide el rendimiento con una foto de tamaño real (12 MP, como un iPhone).
//!
//! `cargo run --release -p renuevo-core --example bench [ruta-a-foto]`

use std::time::Instant;

use image::{DynamicImage, ImageBuffer, Rgb};
use renuevo_core::io::{EncodeOptions, encode};
use renuevo_core::restore::{Preset, RestoreParams};
use renuevo_core::upscale::{UpscaleMethod, UpscaleParams};
use renuevo_core::verify::verify_encoded;
use renuevo_core::{PipelineConfig, Progress, process};

fn synthetic(w: u32, h: u32) -> DynamicImage {
    let mut seed = 0x9E37_79B9_7F4A_7C15u64;
    DynamicImage::ImageRgb8(ImageBuffer::from_fn(w, h, |x, y| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        let noise = (seed >> 58) as i32 - 32;
        let base = [
            (x * 255 / w) as i32,
            (y * 255 / h) as i32,
            (((x / 40 + y / 40) % 2) * 160 + 40) as i32,
        ];
        Rgb(base.map(|v| (v + noise / 3).clamp(0, 255) as u8))
    }))
}

fn time<T>(label: &str, f: impl FnOnce() -> T) -> T {
    let start = Instant::now();
    let out = f();
    println!("{label:<38} {:>7.2} s", start.elapsed().as_secs_f64());
    out
}

fn main() {
    let image = match std::env::args().nth(1) {
        Some(path) => {
            let photo = time("Abrir y decodificar", || {
                renuevo_core::io::open(&path).expect("abrir foto")
            });
            let i = &photo.info;
            println!(
                "{} · {} bits · {} · EXIF {} · GPS {} · orientación aplicada {}",
                i.format.label(),
                i.bits_per_channel,
                i.color.describe(),
                i.had_exif,
                i.had_gps,
                i.orientation_applied
            );
            photo.image
        }
        None => synthetic(4032, 3024),
    };
    println!(
        "Foto: {}×{} ({:.1} MP), {} hilos\n",
        image.width(),
        image.height(),
        f64::from(image.width() * image.height()) / 1e6,
        rayon::current_num_threads()
    );
    let progress = Progress::new();

    let png = time("Copia exacta: codificar PNG", || {
        encode(&image, None, &EncodeOptions::default()).unwrap()
    });
    let report = time("Copia exacta: verificar cada píxel", || {
        verify_encoded(&image, None, &png).unwrap()
    });
    assert!(report.identical);

    let restore = PipelineConfig {
        restore: Some(RestoreParams::preset(Preset::Medium, true)),
        ..Default::default()
    };
    time("Restaurar (Medio, con deblocking)", || {
        process(&image, &restore, &progress).unwrap()
    });

    for method in UpscaleMethod::ALL {
        let cfg = PipelineConfig {
            upscale: Some(UpscaleParams {
                factor: 2.0,
                method,
                sharpen: 0.4,
            }),
            ..Default::default()
        };
        time(&format!("Ampliar ×2 ({method:?}) + RCAS"), || {
            process(&image, &cfg, &progress).unwrap()
        });
    }

    let all = PipelineConfig {
        restore: Some(RestoreParams::preset(Preset::Medium, true)),
        upscale: Some(UpscaleParams {
            factor: 2.0,
            method: UpscaleMethod::EdgeAdaptive,
            sharpen: 0.4,
        }),
        ..Default::default()
    };
    time("Restaurar + ampliar ×2 (EASU)", || {
        process(&image, &all, &progress).unwrap()
    });
}
