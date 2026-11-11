//! Capturas de la interfaz real renderizada con wgpu (Metal en macOS), sin abrir ventana.
//!
//! Necesita GPU, por eso está ignorado por defecto:
//! `RENUEVO_PHOTO=foto.heic RENUEVO_OUT=capturas cargo test -p renuevo-app --test render -- --ignored`

use std::path::PathBuf;
use std::time::{Duration, Instant};

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable as _;
use image::{DynamicImage, Rgb};
use renuevo_app::RenuevoApp;

fn wait_idle(harness: &mut Harness<'_, RenuevoApp>) {
    let start = Instant::now();
    loop {
        harness.step();
        if !harness.state().is_busy() {
            harness.step();
            if !harness.state().is_busy() {
                return;
            }
        }
        assert!(
            start.elapsed() < Duration::from_secs(120),
            "la app no terminó a tiempo"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Degrada la foto (ruido + JPEG calidad 25) para que se note la restauración.
fn degraded_copy(photo: &DynamicImage, out: &std::path::Path) -> PathBuf {
    let mut seed = 0x2545_F491_4F6C_DD1Du64;
    let mut noisy = photo.to_rgb8();
    for p in noisy.pixels_mut() {
        let mut n = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            ((seed >> 59) as i32 - 16) * 2
        };
        *p = Rgb(p.0.map(|v| (i32::from(v) + n()).clamp(0, 255) as u8));
    }
    let path = out.join("degradada.jpg");
    let mut buf = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, 25)
        .encode_image(&noisy)
        .unwrap();
    std::fs::write(&path, buf).unwrap();
    path
}

#[test]
#[ignore = "necesita GPU y una foto: RENUEVO_PHOTO=… RENUEVO_OUT=…"]
fn render_screenshots() {
    let photo_path = PathBuf::from(std::env::var("RENUEVO_PHOTO").expect("RENUEVO_PHOTO"));
    let out = PathBuf::from(std::env::var("RENUEVO_OUT").expect("RENUEVO_OUT"));
    std::fs::create_dir_all(&out).unwrap();
    let original = renuevo_core::io::open(&photo_path).unwrap();
    let input = degraded_copy(&original.image, &out);

    let mut harness = Harness::builder()
        .with_size(egui::vec2(1400.0, 860.0))
        .with_pixels_per_point(1.5)
        .wgpu()
        .build_eframe(|cc| RenuevoApp::new(cc));
    let ctx = harness.ctx.clone();
    let save = |harness: &mut Harness<'_, RenuevoApp>, name: &str| {
        harness.run_steps(3);
        let img = harness.render().expect("render");
        img.save(out.join(name)).unwrap();
    };

    harness.state_mut().open_path(&ctx, input);
    wait_idle(&mut harness);
    save(&mut harness, "1-copia-exacta.png");

    harness
        .get_by_label("Mejorar (restaurar y/o ampliar)")
        .click();
    harness.run_steps(2);
    harness.get_by_label("Fuerte").click();
    harness.run_steps(2);
    harness.get_by_label("Procesar").click();
    harness.step();
    wait_idle(&mut harness);
    save(&mut harness, "2-restaurar-comparar.png");

    harness.get_by_label("Lotes").click();
    save(&mut harness, "3-lotes.png");
}
