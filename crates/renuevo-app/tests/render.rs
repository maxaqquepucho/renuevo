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
fn degraded_copy(photo: &DynamicImage, out: &std::path::Path, name: &str) -> PathBuf {
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
    let path = out.join(name);
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
    let input = degraded_copy(&original.image, &out, "degradada.jpg");

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

    save(&mut harness, "0-vacia.png");
    harness.state_mut().open_path(&ctx, input);
    wait_idle(&mut harness);
    save(&mut harness, "1-copia-exacta.png");

    harness.get_by_label("Mejorar").click();
    harness.run_steps(2);
    harness.get_by_label("Fuerte").click();
    harness.run_steps(2);
    harness.get_by_label("Procesar").click();
    harness.step();
    wait_idle(&mut harness);
    save(&mut harness, "2-restaurar-comparar.png");

    harness.get_by_label("Ampliar").click();
    save(&mut harness, "3-ampliar.png");

    harness.get_by_label("Lotes").click();
    save(&mut harness, "4-lotes.png");

    // El mismo recorrido con el tema claro de macOS.
    ctx.set_theme(egui::Theme::Light);
    save(&mut harness, "5-lotes-claro.png");
    harness.get_by_label("Foto").click();
    save(&mut harness, "6-foto-claro.png");
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
            let (mut grey, mut white, mut total) = (0u64, 0u64, 0u64);
            // Dentro del visor: el panel de ajustes ocupa la derecha de la ventana.
            for y in ih * 35 / 100..ih * 65 / 100 {
                for x in iw * 8 / 100..iw * 52 / 100 {
                    let v = img.get_pixel(x, y)[1];
                    total += 1;
                    grey += u64::from((40..=215).contains(&v));
                    white += u64::from(v > 215);
                }
            }
            let share = grey as f64 / total as f64;
            // Si la zona cayera fuera de la foto (fondo oscuro), no habría gris y el test
            // pasaría sin mirar nada: la mitad del damero tiene que ser blanca.
            let white = white as f64 / total as f64;
            assert!(
                (0.4..=0.6).contains(&white),
                "{w}×{h} a {ppp}×: la zona medida no es el damero ({:.1} % blanco)",
                white * 100.0
            );
            assert!(
                share < 0.01,
                "{w}×{h} a {ppp}×: {:.1} % gris",
                share * 100.0
            );
        }
    }
}

#[derive(Debug)]
struct Dropped(PathBuf);

impl egui::DroppedFile for Dropped {
    fn path(&self) -> &std::path::Path {
        &self.0
    }

    fn bytes(&self) -> Result<Vec<u8>, String> {
        std::fs::read(&self.0).map_err(|e| e.to_string())
    }
}

/// Una foto de móvil con poca luz: ruido moderado y JPEG de calidad 70.
fn phone_copy(photo: &DynamicImage, out: &std::path::Path, name: &str) -> PathBuf {
    let mut seed = 0x9E37_79B9_7F4A_7C15u64;
    let mut noisy = photo.to_rgb8();
    for p in noisy.pixels_mut() {
        let mut n = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed >> 59) as i32 - 16 + ((seed >> 54) & 31) as i32 - 16
        };
        *p = Rgb(p
            .0
            .map(|v| (i32::from(v) + n() * 3 / 4).clamp(0, 255) as u8));
    }
    let path = out.join(name);
    let mut buf = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, 70)
        .encode_image(&noisy)
        .unwrap();
    std::fs::write(&path, buf).unwrap();
    path
}

/// Espera a que llegue el recorte nítido de la vista (se prepara en otro hilo).
fn settle_detail(harness: &mut Harness<'_, RenuevoApp>) {
    for _ in 0..60 {
        harness.step();
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Las capturas del README (`docs/images`), a 1280×800 y densidad 2×. Después,
/// `scripts/frame-screenshots.swift` les pone el marco de ventana de macOS:
///
/// `RENUEVO_PHOTO=foto.jpg RENUEVO_OUT=capturas cargo test --release -p renuevo-app --test render readme -- --ignored`
#[test]
#[ignore = "necesita GPU y una foto: RENUEVO_PHOTO=… RENUEVO_OUT=…"]
fn readme_screenshots() {
    let photo_path = PathBuf::from(std::env::var("RENUEVO_PHOTO").expect("RENUEVO_PHOTO"));
    let out = PathBuf::from(std::env::var("RENUEVO_OUT").expect("RENUEVO_OUT"));
    std::fs::create_dir_all(&out).unwrap();
    let work = tempfile::tempdir().unwrap();
    let original = renuevo_core::io::open(&photo_path).unwrap();
    let input = phone_copy(&original.image, work.path(), "valle.jpg");

    let mut harness = Harness::builder()
        .with_size(egui::vec2(1280.0, 800.0))
        .with_pixels_per_point(2.0)
        .wgpu()
        .build_eframe(|cc| RenuevoApp::new(cc));
    let ctx = harness.ctx.clone();
    let save = |harness: &mut Harness<'_, RenuevoApp>, name: &str| {
        // Sin puntero en la captura.
        harness.event(egui::Event::PointerGone);
        harness.run_steps(3);
        harness
            .render()
            .expect("render")
            .save(out.join(name))
            .unwrap();
    };

    ctx.set_theme(egui::Theme::Dark);
    save(&mut harness, "empty-dark.png");

    // Restaurar una foto con ruido y compresión JPEG, y compararla.
    harness.state_mut().open_path(&ctx, input);
    wait_idle(&mut harness);
    // Con el preajuste «Medio», el de partida.
    harness.get_by_label("Mejorar").click();
    harness.run_steps(2);
    harness.get_by_label("Procesar").click();
    harness.step();
    wait_idle(&mut harness);
    save(&mut harness, "compare-dark.png");

    // El mismo comparador a 1:1: un píxel de la foto por píxel de pantalla.
    harness.get_by_label("1:1").click();
    settle_detail(&mut harness);
    save(&mut harness, "detail-dark.png");

    // Restaurar y ampliar ×2, con el tema claro.
    harness.get_by_label("Ajustar").click();
    ctx.set_theme(egui::Theme::Light);
    harness.get_by_label("Ampliar").click();
    harness.run_steps(2);
    harness.get_by_label("Procesar").click();
    harness.step();
    wait_idle(&mut harness);
    settle_detail(&mut harness);
    save(&mut harness, "upscale-light.png");

    // Una carpeta entera, como copia exacta verificada.
    let folder = work.path().join("viaje");
    std::fs::create_dir_all(&folder).unwrap();
    let (w, h) = (original.image.width(), original.image.height());
    for (i, (x, y)) in [
        (0, 0),
        (1, 0),
        (2, 0),
        (0, 1),
        (1, 1),
        (2, 1),
        (0, 2),
        (1, 2),
    ]
    .into_iter()
    .enumerate()
    {
        let crop = original
            .image
            .crop_imm(x * w / 4, y * h / 4, w / 2, h / 2)
            .to_rgb8();
        let name = format!(
            "IMG_{:04}.{}",
            2041 + i,
            if i % 3 == 2 { "png" } else { "jpg" }
        );
        crop.save(folder.join(name)).unwrap();
    }
    harness.state_mut().settings_mut().mode = renuevo_app::Mode::ExactCopy;
    harness
        .input_mut()
        .dropped_files
        .push(std::sync::Arc::new(Dropped(folder)));
    harness.run_steps(3);
    harness.get_by_label("Procesar 8 fotos").click();
    harness.step();
    wait_idle(&mut harness);
    save(&mut harness, "batch-light.png");
    ctx.set_theme(egui::Theme::Dark);
    save(&mut harness, "batch-dark.png");
}
