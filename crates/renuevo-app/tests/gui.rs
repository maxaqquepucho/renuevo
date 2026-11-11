//! Prueba de extremo a extremo de la interfaz con egui_kittest (sin ventana real).

use std::time::{Duration, Instant};

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable as _;
use renuevo_app::RenuevoApp;

/// Avanza fotogramas hasta que no haya trabajos en segundo plano.
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
            start.elapsed() < Duration::from_secs(60),
            "la app no terminó a tiempo"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn open_exact_copy_enhance_and_export_through_the_gui() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("foto.jpg");
    image::RgbImage::from_fn(320, 240, |x, y| {
        image::Rgb([(x % 256) as u8, (y % 256) as u8, ((x * y) % 251) as u8])
    })
    .save(&input)
    .unwrap();

    let mut harness = Harness::builder()
        .with_size(egui::vec2(1280.0, 820.0))
        .build_eframe(|cc| RenuevoApp::new(cc));
    harness.run_steps(2);
    harness.get_by_label("Abrir foto…");
    let ctx = harness.ctx.clone();

    // Abrir.
    harness.state_mut().open_path(&ctx, input.clone());
    wait_idle(&mut harness);
    assert!(
        harness.state().has_photo(),
        "{:?}",
        harness.state().message_text()
    );

    // Copia exacta verificada.
    let exact = dir.path().join("copia.png");
    harness.state_mut().start_export(&ctx, exact.clone());
    wait_idle(&mut harness);
    let report = harness
        .state()
        .last_export()
        .expect("debería haber exportado");
    assert!(report.verify.identical);
    assert!(!report.verify.exif_present);
    assert!(exact.exists());

    // No se puede sobrescribir el original.
    harness.state_mut().start_export(&ctx, input.clone());
    harness.step();
    assert!(
        harness
            .state()
            .message_text()
            .unwrap_or_default()
            .contains("original")
    );

    // Mejorar desde la interfaz: clic en el modo, activar ampliar ×2 y pulsar Procesar.
    harness
        .get_by_label("Mejorar (restaurar y/o ampliar)")
        .click();
    // Sin pulsar «Procesar», la zona visible se previsualiza sola tras una breve espera.
    harness.run_steps(4);
    wait_idle(&mut harness);
    harness.run_steps(4);
    wait_idle(&mut harness);
    assert!(
        harness.state().has_region_preview(),
        "debería previsualizar la zona visible"
    );

    harness.get_by_label("Ampliar").click();
    harness.run_steps(2);
    assert!(harness.state_mut().settings_mut().upscale_enabled);
    harness.get_by_label("Procesar").click();
    harness.step();
    wait_idle(&mut harness);
    assert!(
        harness.state().has_current_result(),
        "{:?}",
        harness.state().message_text()
    );

    let out = dir.path().join("mejorada.png");
    harness.state_mut().start_export(&ctx, out.clone());
    wait_idle(&mut harness);
    let saved = image::open(&out).unwrap();
    assert_eq!((saved.width(), saved.height()), (640, 480));
    assert_eq!(saved.color(), image::ColorType::Rgb16);
    assert!(harness.state().last_export().unwrap().verify.identical);

    // La pestaña de lotes se muestra con los ajustes actuales.
    harness.get_by_label("Lotes").click();
    harness.run_steps(2);
    harness.get_by_label("Procesar una carpeta");
}

/// Regresión: con una foto de tamaño impar la zona visible empieza en fracción de píxel; la
/// vista previa debe calcularse una vez y no relanzarse sin fin.
#[test]
fn region_preview_does_not_relaunch_forever() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("impar.png");
    image::RgbImage::from_fn(301, 201, |x, y| {
        image::Rgb([(x % 256) as u8, (y % 256) as u8, ((x ^ y) % 256) as u8])
    })
    .save(&input)
    .unwrap();

    let mut harness = Harness::builder()
        .with_size(egui::vec2(1100.0, 760.0))
        .build_eframe(|cc| RenuevoApp::new(cc));
    harness.run_steps(2);
    let ctx = harness.ctx.clone();
    harness.state_mut().open_path(&ctx, input);
    wait_idle(&mut harness);
    harness
        .get_by_label("Mejorar (restaurar y/o ampliar)")
        .click();
    harness.get_by_label("1:1").click();
    for _ in 0..3 {
        harness.run_steps(4);
        wait_idle(&mut harness);
    }
    assert!(harness.state().has_region_preview());
    let runs = harness.state().region_preview_runs();
    for _ in 0..30 {
        harness.step();
        std::thread::sleep(Duration::from_millis(5));
    }
    wait_idle(&mut harness);
    assert_eq!(
        harness.state().region_preview_runs(),
        runs,
        "la vista previa se relanzó sin ninguna interacción"
    );
}

#[derive(Debug)]
struct Dropped(std::path::PathBuf);

impl egui::DroppedFile for Dropped {
    fn path(&self) -> &std::path::Path {
        &self.0
    }

    fn bytes(&self) -> Result<Vec<u8>, String> {
        std::fs::read(&self.0).map_err(|e| e.to_string())
    }
}

fn drop_path(harness: &mut Harness<'_, RenuevoApp>, path: std::path::PathBuf) {
    harness
        .input_mut()
        .dropped_files
        .push(std::sync::Arc::new(Dropped(path)));
    harness.step();
}

/// Regresión: con un lote en marcha, soltar otra carpeta cambiaba la lista a mitad del lote y
/// soltar una foto la abría; el aviso no se veía y luego aparecía cuando ya no era verdad.
#[test]
fn batch_in_progress_blocks_drops_and_its_warning_clears_after() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = (dir.path().join("a"), dir.path().join("b"));
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    let photo = |x0: u32| {
        image::RgbImage::from_fn(900, 700, move |x, y| {
            image::Rgb([
                ((x + x0) % 256) as u8,
                (y % 256) as u8,
                ((x * y) % 251) as u8,
            ])
        })
    };
    for i in 0..6 {
        photo(i).save(a.join(format!("f{i}.png"))).unwrap();
    }
    photo(9).save(b.join("otra.png")).unwrap();
    let single = dir.path().join("suelta.png");
    photo(3).save(&single).unwrap();

    let mut harness = Harness::builder()
        .with_size(egui::vec2(1280.0, 820.0))
        .build_eframe(|cc| RenuevoApp::new(cc));
    harness.run_steps(2);
    harness.state_mut().settings_mut().mode = renuevo_app::Mode::Enhance;

    drop_path(&mut harness, a.clone());
    harness.run_steps(2);
    assert_eq!(harness.state().batch_file_count(), 6);
    harness.get_by_label("Procesar 6 fotos").click();
    harness.run_steps(2);
    assert!(
        harness.state().batch_running(),
        "el lote debería estar en marcha"
    );

    // Otra carpeta y una foto suelta durante el lote: se rechazan con un aviso visible.
    drop_path(&mut harness, b);
    drop_path(&mut harness, single);
    harness.run_steps(2);
    assert_eq!(
        harness.state().batch_file_count(),
        6,
        "la lista cambió a mitad del lote"
    );
    assert!(
        !harness.state().has_photo(),
        "se abrió una foto durante el lote"
    );
    let warning = harness
        .state()
        .message_text()
        .unwrap_or_default()
        .to_owned();
    assert!(warning.contains("lote en marcha"), "{warning:?}");
    harness.get_by_label(&warning);

    // Al terminar el lote, el aviso desaparece (ya no es verdad).
    wait_idle(&mut harness);
    assert!(!harness.state().batch_running());
    assert_eq!(harness.state().message_text(), None);
    assert_eq!(std::fs::read_dir(a.join("renuevo")).unwrap().count(), 6);
}
