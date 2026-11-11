//! `renuevo [foto]`: abre la app (y opcionalmente una foto).

fn main() -> eframe::Result {
    let initial_file = std::env::args_os()
        .nth(1)
        .map(|p| std::path::absolute(&p).unwrap_or_else(|_| p.into()));
    // Sin icono propio, eframe pone el de egui en el Dock, también dentro de Renuevo.app.
    let icon = eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon.png"))
        .expect("assets/icon.png es un PNG válido");
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Renuevo")
            .with_icon(icon)
            .with_inner_size([1400.0, 900.0])
            .with_min_inner_size([900.0, 600.0])
            .with_drag_and_drop(true),
        ..Default::default()
    };
    eframe::run_native(
        "Renuevo",
        options,
        Box::new(|cc| {
            Ok(Box::new(
                renuevo_app::RenuevoApp::new(cc).with_initial_file(initial_file),
            ))
        }),
    )
}
