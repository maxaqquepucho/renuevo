//! `renuevo [foto]`: abre la app (y opcionalmente una foto).

fn main() -> eframe::Result {
    let initial_file = std::env::args_os()
        .nth(1)
        .map(|p| std::path::absolute(&p).unwrap_or_else(|_| p.into()));
    // Sin icono propio, eframe pone el de egui en el Dock, también dentro de Renuevo.app.
    let icon = eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon.png"))
        .expect("assets/icon.png es un PNG válido");
    let viewport = egui::ViewportBuilder::default()
        .with_title("Renuevo")
        .with_icon(icon)
        .with_inner_size([1400.0, 900.0])
        .with_min_inner_size([960.0, 620.0])
        .with_drag_and_drop(true)
        // Barra de título unificada, como las apps de Apple: los botones de la ventana quedan
        // sobre la barra de herramientas de la app (ver `app::TOOLBAR_HEIGHT`).
        .with_fullsize_content_view(true)
        .with_titlebar_shown(false)
        .with_title_shown(false);
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native(
        "Renuevo",
        options,
        Box::new(|cc| {
            let app = renuevo_app::RenuevoApp::new(cc).with_initial_file(initial_file);
            #[cfg(target_os = "macos")]
            let app = app.with_native_menu(&cc.egui_ctx);
            Ok(Box::new(app))
        }),
    )
}
