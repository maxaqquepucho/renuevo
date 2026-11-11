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

/// A 1:1 cada píxel de la foto debe caer en un píxel físico: un damero de 1 px no puede salir
/// gris (mezclado por el filtrado) sea cual sea la paridad del tamaño o la densidad de pantalla.
#[test]
#[ignore = "necesita GPU"]
fn one_to_one_is_sharp_whatever_the_size_parity() {
    let dir = tempfile::tempdir().unwrap();
    for ppp in [1.0f32, 2.0] {
        for (w, h) in [(3000u32, 2000u32), (3001, 2001), (3000, 2001)] {
            let path = dir.path().join(format!("damero_{w}x{h}.png"));
            image::RgbImage::from_fn(
                w,
                h,
                |x, y| Rgb([if (x + y) % 2 == 0 { 255 } else { 0 }; 3]),
            )
            .save(&path)
            .unwrap();
            let mut harness = Harness::builder()
                .with_size(egui::vec2(1000.0, 640.0))
                .with_pixels_per_point(ppp)
                .wgpu()
                .build_eframe(|cc| RenuevoApp::new(cc));
            let ctx = harness.ctx.clone();
            harness.state_mut().open_path(&ctx, path);
            wait_idle(&mut harness);
            harness.get_by_label("1:1").click();
            // El recorte nítido se prepara en otro hilo.
            for _ in 0..60 {
                harness.step();
                std::thread::sleep(Duration::from_millis(20));
            }
            let img = harness.render().expect("render");
            let (iw, ih) = img.dimensions();
            let (mut grey, mut total) = (0u64, 0u64);
            for y in ih * 35 / 100..ih * 65 / 100 {
                for x in iw * 62 / 100..iw * 92 / 100 {
                    total += 1;
                    grey += u64::from((40..=215).contains(&img.get_pixel(x, y)[1]));
                }
            }
            let share = grey as f64 / total as f64;
            assert!(
                share < 0.01,
                "{w}×{h} a {ppp}×: {:.1} % gris",
                share * 100.0
            );
        }
    }
}
