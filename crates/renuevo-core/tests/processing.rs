//! Fases 3-5: restaurar, ampliar, pipeline y lotes, con métricas objetivas (PSNR/SSIM).

mod common;

use image::imageops::FilterType;
use image::{DynamicImage, ImageBuffer, Rgb};
use renuevo_core::batch::{BatchJob, ItemStatus, list_images, run_batch};
use renuevo_core::io::{EncodeOptions, OutputFormat};
use renuevo_core::metrics::{psnr, ssim};
use renuevo_core::planar::Planar;
use renuevo_core::restore::{Preset, RestoreParams, noise::estimate_sigma, restore};
use renuevo_core::upscale::{UpscaleMethod, UpscaleParams, upscale};
use renuevo_core::{BitDepth, Error, PipelineConfig, Progress, color, process, process_region};

/// "Foto" sintética: degradados suaves, círculos con borde suave, líneas diagonales y textura fina.
fn synthetic_photo(w: u32, h: u32) -> DynamicImage {
    DynamicImage::ImageRgb8(ImageBuffer::from_fn(w, h, |x, y| {
        let (fx, fy) = (x as f32 / w as f32, y as f32 / h as f32);
        let mut r = 0.25 + 0.5 * fx;
        let mut g = 0.3 + 0.4 * fy;
        let mut b = 0.6 - 0.3 * fx * fy;
        // Círculo con borde suavizado.
        let d = ((fx - 0.35).powi(2) + (fy - 0.4).powi(2)).sqrt();
        let disk = (1.0 - ((d - 0.2) * 60.0).clamp(0.0, 1.0)) * 0.6;
        r += disk * 0.5;
        g -= disk * 0.2;
        // Franjas diagonales (bordes oblicuos).
        let stripe = (((x + y) as f32 * 0.09).sin() > 0.6) as u8 as f32;
        if fx > 0.6 {
            b += stripe * 0.25;
        }
        // Textura fina determinista.
        let t = ((x.wrapping_mul(2_654_435_761) ^ y.wrapping_mul(40_503)) % 17) as f32 / 17.0 - 0.5;
        let q = |v: f32| ((v + t * 0.02).clamp(0.0, 1.0) * 255.0).round() as u8;
        Rgb([q(r), q(g), q(b)])
    }))
}

/// Generador xorshift + Box-Muller (determinista, sin dependencias).
struct Rng(u64);
impl Rng {
    fn next_f32(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        ((self.0 >> 40) as f32 + 0.5) / (1u64 << 24) as f32
    }
    fn gaussian(&mut self) -> f32 {
        let (u1, u2) = (self.next_f32(), self.next_f32());
        (-2.0 * u1.ln()).sqrt() * (std::f32::consts::TAU * u2).cos()
    }
}

fn add_noise(image: &DynamicImage, sigma: f32, seed: u64) -> DynamicImage {
    let mut rng = Rng(seed);
    let rgb = image.to_rgb8();
    DynamicImage::ImageRgb8(ImageBuffer::from_fn(rgb.width(), rgb.height(), |x, y| {
        let p = rgb.get_pixel(x, y).0;
        Rgb(p.map(|v| {
            ((f32::from(v) / 255.0 + rng.gaussian() * sigma).clamp(0.0, 1.0) * 255.0).round() as u8
        }))
    }))
}

fn jpeg_roundtrip(image: &DynamicImage, quality: u8) -> DynamicImage {
    let mut buf = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, quality)
        .encode_image(image)
        .unwrap();
    image::load_from_memory(&buf).unwrap()
}

fn restore_image(
    image: &DynamicImage,
    params: RestoreParams,
) -> (DynamicImage, renuevo_core::restore::RestoreReport) {
    let mut planar = Planar::from_image(image);
    let report = restore(&mut planar, &params, None, &Progress::new()).unwrap();
    (planar.to_image(BitDepth::Bits8), report)
}

#[test]
fn planar_roundtrip_is_exact() {
    for (image, depth) in [
        (common::rgb8(53, 41), BitDepth::Bits8),
        (common::rgba8(40, 30), BitDepth::Bits8),
        (common::rgb16(53, 41), BitDepth::Bits16),
        (common::luma16(31, 29), BitDepth::Bits16),
    ] {
        let back = Planar::from_image(&image).to_image(depth);
        assert_eq!(back.color(), image.color());
        assert_eq!(back.as_bytes(), image.as_bytes(), "{:?}", image.color());
    }
}

#[test]
fn ycbcr_roundtrip_is_lossless_in_f32() {
    let original = Planar::from_image(&common::rgb16(64, 48));
    let mut p = original.clone();
    color::rgb_to_ycbcr(&mut p);
    color::ycbcr_to_rgb(&mut p);
    for c in 0..3 {
        let max_err = p.channels[c]
            .iter()
            .zip(&original.channels[c])
            .map(|(a, b)| (a - b).abs())
            .fold(0.0f32, f32::max);
        assert!(max_err < 1e-5, "canal {c}: {max_err}");
    }
}

#[test]
fn noise_estimate_tracks_real_noise() {
    let clean = synthetic_photo(256, 256);
    let luma = |img: &DynamicImage| {
        let mut p = Planar::from_image(img);
        color::rgb_to_ycbcr(&mut p);
        p.channels[0].clone()
    };
    let clean_sigma = estimate_sigma(&luma(&clean), 256, 256);
    // σ por canal 0.03 → en luma ≈ 0.03·√(0.299²+0.587²+0.114²) ≈ 0.0201
    let noisy_sigma = estimate_sigma(&luma(&add_noise(&clean, 0.03, 7)), 256, 256);
    assert!(clean_sigma < 0.008, "foto limpia: σ={clean_sigma}");
    assert!(
        (0.014..0.028).contains(&noisy_sigma),
        "foto con ruido: σ={noisy_sigma}"
    );
}

#[test]
fn restore_reduces_gaussian_noise() {
    let clean = synthetic_photo(256, 192);
    let noisy = add_noise(&clean, 0.04, 42);
    let (restored, report) = restore_image(&noisy, RestoreParams::preset(Preset::Medium, false));

    let (psnr_before, psnr_after) = (psnr(&clean, &noisy), psnr(&clean, &restored));
    let (ssim_before, ssim_after) = (ssim(&clean, &noisy), ssim(&clean, &restored));
    println!(
        "ruido: PSNR {psnr_before:.2} → {psnr_after:.2} dB, SSIM {ssim_before:.3} → {ssim_after:.3}, σ≈{:.1}/255",
        report.noise_sigma_8bit
    );
    assert!(
        psnr_after > psnr_before + 3.0,
        "PSNR {psnr_before:.2} → {psnr_after:.2}"
    );
    assert!(
        ssim_after > ssim_before + 0.05,
        "SSIM {ssim_before:.3} → {ssim_after:.3}"
    );
}

#[test]
fn restore_leaves_clean_photo_almost_untouched() {
    let clean = synthetic_photo(200, 150);
    let (restored, _) = restore_image(&clean, RestoreParams::preset(Preset::Soft, false));
    let p = psnr(&clean, &restored);
    assert!(
        p > 34.0,
        "una foto limpia no debería cambiar mucho: {p:.2} dB"
    );
}

/// Regresión: la compresión JPEG convierte el ruido en manchas que un estimador de alta
/// frecuencia no ve. El estimador por bloques sí debe distinguir foto ruidosa de foto limpia.
#[test]
fn noise_estimate_sees_noise_after_jpeg_compression() {
    let clean = synthetic_photo(256, 256);
    let noisy_jpeg = jpeg_roundtrip(&add_noise(&clean, 0.05, 3), 30);
    let clean_jpeg = jpeg_roundtrip(&clean, 60);
    let (_, noisy) = restore_image(&noisy_jpeg, RestoreParams::preset(Preset::Soft, true));
    let (_, clean) = restore_image(&clean_jpeg, RestoreParams::preset(Preset::Soft, true));
    println!(
        "σ JPEG ruidoso {:.2} · σ JPEG limpio {:.2}",
        noisy.noise_sigma_8bit, clean.noise_sigma_8bit
    );
    assert!(noisy.noise_sigma_8bit > 3.0 * clean.noise_sigma_8bit.max(0.5));
}

/// Regresión: con ruido «manchado» por JPEG la restauración debe mejorar la foto…
#[test]
fn restore_improves_noisy_jpeg() {
    let clean = synthetic_photo(256, 256);
    let degraded = jpeg_roundtrip(&add_noise(&clean, 0.05, 11), 30);
    let (restored, _) = restore_image(&degraded, RestoreParams::preset(Preset::Medium, true));
    let (before, after) = (psnr(&clean, &degraded), psnr(&clean, &restored));
    println!("JPEG ruidoso: PSNR {before:.2} → {after:.2} dB");
    assert!(after > before + 1.0, "PSNR {before:.2} → {after:.2}");
}

/// …y casi no tocar una foto limpia con compresión suave (antes NLM la emborronaba porque el
/// píxel central pesaba como su mejor vecino, aunque fuera muy distinto).
#[test]
fn restore_barely_changes_clean_jpeg() {
    let clean = synthetic_photo(256, 256);
    let jpeg = jpeg_roundtrip(&clean, 85);
    let (restored, _) = restore_image(&jpeg, RestoreParams::preset(Preset::Medium, true));
    let (before, after) = (psnr(&clean, &jpeg), psnr(&clean, &restored));
    println!("JPEG limpio: PSNR {before:.2} → {after:.2} dB");
    assert!(after > before - 0.5, "PSNR {before:.2} → {after:.2}");
}

#[test]
fn deblocking_improves_heavily_compressed_jpeg() {
    let clean = synthetic_photo(256, 256);
    let blocky = jpeg_roundtrip(&clean, 12);
    let params = RestoreParams {
        deblock: 1.0,
        chroma_noise: 0.0,
        luma_noise: 0.0,
        sharpen: 0.0,
    };
    let (restored, report) = restore_image(&blocky, params);
    let (before, after) = (psnr(&clean, &blocky), psnr(&clean, &restored));
    println!(
        "JPEG q12: PSNR {before:.2} → {after:.2} dB, SSIM {:.4} → {:.4}",
        ssim(&clean, &blocky),
        ssim(&clean, &restored)
    );
    assert!(report.blocking_detected, "debería detectar la rejilla 8×8");
    assert!(after > before, "PSNR {before:.2} → {after:.2}");
}

#[test]
fn upscalers_beat_bilinear() {
    let original = synthetic_photo(256, 256);
    let small = original.resize_exact(128, 128, FilterType::Lanczos3);
    let bilinear = small.resize_exact(256, 256, FilterType::Triangle);
    let base = psnr(&original, &bilinear);

    for method in UpscaleMethod::ALL {
        let params = UpscaleParams {
            factor: 2.0,
            method,
            sharpen: 0.0,
        };
        let up = upscale(&Planar::from_image(&small), &params, &Progress::new()).unwrap();
        assert_eq!((up.width, up.height), (256, 256));
        let ours = psnr(&original, &up.to_image(BitDepth::Bits8));
        println!("{method:?}: {ours:.2} dB (bilineal {base:.2} dB)");
        assert!(
            ours > base,
            "{method:?}: {ours:.2} dB no supera al bilineal ({base:.2} dB)"
        );
    }
}

#[test]
fn edge_adaptive_upscale_is_stable_on_flat_images_and_any_factor() {
    let flat = DynamicImage::ImageRgb8(ImageBuffer::from_pixel(40, 30, Rgb([120, 60, 200])));
    for factor in [1.5, 2.0, 3.0, 4.0] {
        let params = UpscaleParams {
            factor,
            method: UpscaleMethod::EdgeAdaptive,
            sharpen: 0.8,
        };
        let up = upscale(&Planar::from_image(&flat), &params, &Progress::new()).unwrap();
        assert_eq!(
            (up.width, up.height),
            ((40.0 * factor) as usize, (30.0 * factor) as usize)
        );
        let out = up.to_image(BitDepth::Bits8).to_rgb8();
        assert!(
            out.pixels().all(|p| p.0 == [120, 60, 200]),
            "factor {factor}"
        );
    }
}

#[test]
fn exact_copy_pipeline_does_not_touch_pixels() {
    let image = common::rgb16(64, 64);
    let processed = process(&image, &PipelineConfig::exact_copy(), &Progress::new()).unwrap();
    assert_eq!(processed.image.as_bytes(), image.as_bytes());
    assert_eq!(processed.image.color(), image.color());
}

#[test]
fn pipeline_outputs_16_bits_and_respects_size_limit() {
    let image = synthetic_photo(64, 48);
    let mut cfg = PipelineConfig {
        upscale: Some(UpscaleParams::default()),
        restore: Some(RestoreParams::preset(Preset::Soft, false)),
        ..Default::default()
    };
    let out = process(&image, &cfg, &Progress::new()).unwrap();
    assert_eq!((out.image.width(), out.image.height()), (128, 96));
    assert_eq!(out.image.color(), image::ColorType::Rgb16);
    assert_eq!(
        cfg.for_format(OutputFormat::WebP).output_depth,
        BitDepth::Bits8
    );

    cfg.max_output_megapixels = 0.001;
    let err = process(&image, &cfg, &Progress::new()).unwrap_err();
    assert!(matches!(err, Error::TooLarge(_)), "{err}");
}

#[test]
fn region_preview_has_the_scaled_size() {
    let image = synthetic_photo(200, 160);
    let cfg = PipelineConfig {
        upscale: Some(UpscaleParams {
            factor: 2.0,
            ..Default::default()
        }),
        restore: Some(RestoreParams::preset(Preset::Medium, false)),
        ..Default::default()
    };
    let region = process_region(&image, &cfg, (10, 20, 50, 40), &Progress::new()).unwrap();
    assert_eq!((region.width(), region.height()), (100, 80));
}

#[test]
fn processing_can_be_cancelled() {
    let image = synthetic_photo(128, 128);
    let cfg = PipelineConfig {
        restore: Some(RestoreParams::preset(Preset::Strong, false)),
        ..Default::default()
    };
    let progress = Progress::new();
    progress.cancel();
    assert!(matches!(
        process(&image, &cfg, &progress),
        Err(Error::Cancelled)
    ));
}

#[test]
fn batch_processes_folder_skips_existing_and_never_overwrites_originals() {
    let dir = tempfile::tempdir().unwrap();
    let (input, output) = (dir.path().join("in"), dir.path().join("out"));
    std::fs::create_dir_all(&input).unwrap();
    std::fs::create_dir_all(&output).unwrap();
    synthetic_photo(64, 48).save(input.join("a.png")).unwrap();
    synthetic_photo(48, 64)
        .to_rgb8()
        .save(input.join("b.jpg"))
        .unwrap();
    std::fs::write(input.join("notas.txt"), "no es una foto").unwrap();

    let inputs = list_images(&input).unwrap();
    assert_eq!(inputs.len(), 2, "solo las fotos: {inputs:?}");

    let mut job = BatchJob {
        inputs,
        output_dir: output.clone(),
        suffix: "_renuevo".to_owned(),
        skip_existing: true,
        pipeline: PipelineConfig::exact_copy(),
        encode: EncodeOptions::default(),
    };
    let mut events = Vec::new();
    let summary = run_batch(&job, &Progress::new(), |i, s| events.push((i, s)));
    assert_eq!((summary.done, summary.failed), (2, 0), "{events:?}");
    assert!(output.join("a_renuevo.png").exists());
    assert!(output.join("b_renuevo.png").exists());

    // Segunda vez: ya existen → se omiten.
    let summary = run_batch(&job, &Progress::new(), |_, _| {});
    assert_eq!(summary.skipped, 2);

    // Misma carpeta, sin sufijo y mismo formato: a.png se pisaría a sí misma → se omite.
    job.output_dir = input.clone();
    job.suffix.clear();
    job.skip_existing = false;
    let mut statuses = Vec::new();
    run_batch(&job, &Progress::new(), |i, s| statuses.push((i, s)));
    assert!(
        statuses.contains(&(0, ItemStatus::Skipped("sobrescribiría un original".into()))),
        "{statuses:?}"
    );
    assert_eq!(
        image::open(input.join("a.png")).unwrap().width(),
        64,
        "el original sigue intacto"
    );
}

fn file_bytes(path: &std::path::Path) -> Vec<u8> {
    std::fs::read(path).unwrap()
}

/// Regresión: la salida de una foto no puede pisar otra foto de entrada del mismo lote.
#[test]
fn batch_never_overwrites_other_inputs() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    // A: «a_renuevo.png» es una entrada y a la vez la salida de «a.png».
    synthetic_photo(40, 30).save(d.join("a.png")).unwrap();
    synthetic_photo(30, 40)
        .save(d.join("a_renuevo.png"))
        .unwrap();
    // B: con sufijo vacío, la salida de «foto.jpg» sería «foto.png», otra entrada.
    synthetic_photo(50, 20)
        .to_rgb8()
        .save(d.join("foto.jpg"))
        .unwrap();
    synthetic_photo(20, 50).save(d.join("foto.png")).unwrap();
    let before: Vec<_> = ["a.png", "a_renuevo.png", "foto.jpg", "foto.png"]
        .iter()
        .map(|n| file_bytes(&d.join(n)))
        .collect();

    for suffix in ["_renuevo", ""] {
        let job = BatchJob {
            inputs: list_images(d).unwrap(),
            output_dir: d.to_path_buf(),
            suffix: suffix.to_owned(),
            skip_existing: false,
            pipeline: PipelineConfig::exact_copy(),
            encode: EncodeOptions::default(),
        };
        run_batch(&job, &Progress::new(), |_, _| {});
        let after: Vec<_> = ["a.png", "a_renuevo.png", "foto.jpg", "foto.png"]
            .iter()
            .map(|n| file_bytes(&d.join(n)))
            .collect();
        assert_eq!(before, after, "sufijo {suffix:?}: se modificó un original");
    }
}

/// Regresión: «a.jpg» y «a.png» no pueden acabar en la misma salida.
#[test]
fn batch_gives_distinct_outputs_to_same_stem() {
    let dir = tempfile::tempdir().unwrap();
    let (input, output) = (dir.path().join("in"), dir.path().join("out"));
    std::fs::create_dir_all(&input).unwrap();
    synthetic_photo(40, 30)
        .to_rgb8()
        .save(input.join("a.jpg"))
        .unwrap();
    synthetic_photo(30, 40).save(input.join("a.png")).unwrap();
    let job = BatchJob {
        inputs: list_images(&input).unwrap(),
        output_dir: output.clone(),
        suffix: "_renuevo".to_owned(),
        skip_existing: true,
        pipeline: PipelineConfig::exact_copy(),
        encode: EncodeOptions::default(),
    };
    std::fs::create_dir_all(&output).unwrap();
    let summary = run_batch(&job, &Progress::new(), |_, _| {});
    assert_eq!((summary.done, summary.skipped), (2, 0));
    assert_eq!(
        image::open(output.join("a_jpg_renuevo.png"))
            .unwrap()
            .width(),
        40
    );
    assert_eq!(
        image::open(output.join("a_png_renuevo.png"))
            .unwrap()
            .width(),
        30
    );
}

/// Regresión: un NaN/∞ en un TIFF en coma flotante no puede tumbar el procesado.
#[test]
fn non_finite_float_pixels_do_not_panic() {
    let mut img = image::Rgb32FImage::from_pixel(32, 24, image::Rgb([0.4, 0.5, 0.6]));
    img.put_pixel(
        5,
        5,
        image::Rgb([f32::NAN, f32::INFINITY, f32::NEG_INFINITY]),
    );
    let image = DynamicImage::ImageRgb32F(img);
    let cfg = PipelineConfig {
        restore: Some(RestoreParams::preset(Preset::Strong, false)),
        upscale: Some(UpscaleParams {
            factor: 2.0,
            method: UpscaleMethod::EdgeAdaptive,
            sharpen: 1.0,
        }),
        ..Default::default()
    };
    let out = process(&image, &cfg, &Progress::new()).unwrap();
    assert_eq!((out.image.width(), out.image.height()), (64, 48));
}

/// Regresión: factores absurdos se rechazan en lugar de truncarse a 0×0 o 1×1.
#[test]
fn invalid_upscale_factors_are_rejected() {
    let image = synthetic_photo(8, 8);
    for factor in [f32::NAN, f32::INFINITY, -2.0, 0.0, 1e9] {
        let cfg = PipelineConfig {
            upscale: Some(UpscaleParams {
                factor,
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(
            process(&image, &cfg, &Progress::new()).is_err(),
            "factor {factor}"
        );
    }
}

/// La vista previa de una zona coincide con la misma zona de la foto procesada entera
/// (mismo ruido estimado, misma rejilla), incluso con ×1,5 y origen impar.
#[test]
fn region_preview_matches_full_processing() {
    let image = add_noise(&synthetic_photo(160, 120), 0.03, 5);
    let noise = renuevo_core::restore::estimate_noise(&image);
    for factor in [1.5f32, 2.0] {
        let cfg = PipelineConfig {
            restore: Some(RestoreParams::preset(Preset::Medium, false)),
            upscale: Some(UpscaleParams {
                factor,
                method: UpscaleMethod::EdgeAdaptive,
                sharpen: 0.3,
            }),
            noise: Some(noise),
            output_depth: BitDepth::Bits8,
            ..Default::default()
        };
        let full = process(&image, &cfg, &Progress::new()).unwrap().image;
        let (x, y, w, h) = (61, 37, 40, 30);
        let region = process_region(&image, &cfg, (x, y, w, h), &Progress::new()).unwrap();
        let s = |v: u32| (v as f32 * factor).round() as u32;
        let same = full.crop_imm(s(x), s(y), region.width(), region.height());
        let p = psnr(&same, &region);
        println!("zona ×{factor}: {p:.1} dB frente a la foto completa");
        assert!(p > 45.0, "×{factor}: la vista previa difiere ({p:.1} dB)");
    }
}
