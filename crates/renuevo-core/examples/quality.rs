//! Mide cuánto mejora (o empeora) cada etapa de la restauración frente a una referencia limpia.
//!
//! `cargo run --release -p renuevo-core --example quality -- original.heic degradada.jpg [salida/]`

use renuevo_core::metrics::{psnr, ssim};
use renuevo_core::restore::{Preset, RestoreParams};
use renuevo_core::{PipelineConfig, Progress, process};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let clean = renuevo_core::io::open(&args[1]).unwrap().image;
    let degraded_photo = renuevo_core::io::open(&args[2]).unwrap();
    let degraded = degraded_photo.image;
    let out_dir = args.get(3);
    let is_jpeg = degraded_photo.info.format == renuevo_core::SourceFormat::Jpeg;

    let only = |deblock, chroma_noise, luma_noise, sharpen| RestoreParams {
        deblock,
        chroma_noise,
        luma_noise,
        sharpen,
    };
    let mut cases: Vec<(String, RestoreParams)> = vec![
        ("solo bloques".into(), only(1.0, 0.0, 0.0, 0.0)),
        ("solo color".into(), only(0.0, 0.9, 0.0, 0.0)),
        ("solo grano".into(), only(0.0, 0.0, 0.85, 0.0)),
        ("solo enfoque".into(), only(0.0, 0.0, 0.0, 0.3)),
    ];
    for p in Preset::ALL {
        cases.push((
            format!("preset {}", p.label()),
            RestoreParams::preset(p, is_jpeg),
        ));
    }

    println!("{:<18} {:>8} {:>8}", "", "PSNR", "SSIM");
    println!(
        "{:<18} {:>8.2} {:>8.4}",
        "degradada",
        psnr(&clean, &degraded),
        ssim(&clean, &degraded)
    );
    for (name, params) in cases {
        let cfg = PipelineConfig {
            restore: Some(params),
            output_depth: renuevo_core::BitDepth::Bits8,
            ..Default::default()
        };
        let out = process(&degraded, &cfg, &Progress::new()).unwrap();
        let (sigma, chroma) = out
            .restore_report
            .as_ref()
            .map_or((0.0, 0.0), |r| (r.noise_sigma_8bit, r.chroma_sigma_8bit));
        println!(
            "{name:<18} {:>8.2} {:>8.4}   (σ luma {sigma:.1}, color {chroma:.1})",
            psnr(&clean, &out.image),
            ssim(&clean, &out.image)
        );
        if let Some(dir) = out_dir {
            out.image
                .save(format!("{dir}/{}.png", name.replace(' ', "_")))
                .unwrap();
        }
    }
}
