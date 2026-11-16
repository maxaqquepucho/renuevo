//! Barra de menús nativa de macOS (Renuevo, Archivo, Ver, Ventana) con `muda`.
//!
//! Los menús son de AppKit de verdad: sus atajos los gestiona macOS, que los muestra en cada
//! opción. Las órdenes llegan por un canal y se atienden en el siguiente fotograma.

use std::sync::mpsc::{self, Receiver};

use muda::{
    AboutMetadata, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem, Submenu,
    accelerator::Accelerator,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Open,
    OpenFolder,
    Export,
    ShowPhoto,
    ShowBatch,
    Fit,
    OneToOne,
}

/// Qué opciones se pueden usar ahora.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MenuState {
    pub can_open: bool,
    pub can_export: bool,
    pub has_photo: bool,
}

pub struct NativeMenu {
    // Hay que mantener vivo el menú mientras la app lo usa.
    _menu: Menu,
    items: Vec<(MenuItem, Command)>,
    rx: Receiver<MenuId>,
    state: Option<MenuState>,
}

fn item(text: &str, accelerator: Option<&str>) -> MenuItem {
    let accelerator = accelerator.map(|a| {
        a.parse::<Accelerator>()
            .expect("los atajos del menú están bien escritos")
    });
    MenuItem::new(text, true, accelerator)
}

impl NativeMenu {
    /// Crea la barra de menús y la instala. Solo en el hilo principal, con la app ya abierta
    /// (sustituye al menú mínimo que pone winit).
    pub fn install(ctx: &egui::Context) -> Result<Self, muda::Error> {
        let items = vec![
            (item("Abrir…", Some("Cmd+O")), Command::Open),
            (
                item("Abrir carpeta para lotes…", Some("Cmd+Shift+O")),
                Command::OpenFolder,
            ),
            (item("Exportar…", Some("Cmd+S")), Command::Export),
            (item("Foto", Some("Cmd+1")), Command::ShowPhoto),
            (item("Lotes", Some("Cmd+2")), Command::ShowBatch),
            (item("Ajustar a la ventana", Some("Cmd+9")), Command::Fit),
            (item("Tamaño real (1:1)", Some("Cmd+0")), Command::OneToOne),
        ];
        let get = |c: Command| &items.iter().find(|(_, k)| *k == c).expect("opción").0;

        let about = AboutMetadata {
            name: Some("Renuevo".to_owned()),
            version: Some(env!("CARGO_PKG_VERSION").to_owned()),
            comments: Some("Copia, restaura y amplía fotos sin perder calidad.".to_owned()),
            ..Default::default()
        };
        let app_menu = Submenu::with_items(
            "Renuevo",
            true,
            &[
                &PredefinedMenuItem::about(Some("Acerca de Renuevo"), Some(about)),
                &PredefinedMenuItem::separator(),
                &PredefinedMenuItem::services(Some("Servicios")),
                &PredefinedMenuItem::separator(),
                &PredefinedMenuItem::hide(Some("Ocultar Renuevo")),
                &PredefinedMenuItem::hide_others(Some("Ocultar otros")),
                &PredefinedMenuItem::show_all(Some("Mostrar todo")),
                &PredefinedMenuItem::separator(),
                &PredefinedMenuItem::quit(Some("Salir de Renuevo")),
            ],
        )?;
        let file = Submenu::with_items(
            "Archivo",
            true,
            &[
                get(Command::Open),
                get(Command::OpenFolder),
                &PredefinedMenuItem::separator(),
                get(Command::Export),
                &PredefinedMenuItem::separator(),
                &PredefinedMenuItem::close_window(Some("Cerrar ventana")),
            ],
        )?;
        let view = Submenu::with_items(
            "Ver",
            true,
            &[
                get(Command::ShowPhoto),
                get(Command::ShowBatch),
                &PredefinedMenuItem::separator(),
                get(Command::Fit),
                get(Command::OneToOne),
                &PredefinedMenuItem::separator(),
                &PredefinedMenuItem::fullscreen(Some("Pantalla completa")),
            ],
        )?;
        let window = Submenu::with_items(
            "Ventana",
            true,
            &[
                &PredefinedMenuItem::minimize(Some("Minimizar")),
                &PredefinedMenuItem::maximize(Some("Zoom")),
                &PredefinedMenuItem::separator(),
                &PredefinedMenuItem::bring_all_to_front(Some("Traer todo al frente")),
            ],
        )?;
        let menu = Menu::with_items(&[&app_menu, &file, &view, &window])?;
        menu.init_for_nsapp();
        // macOS añade a este menú la lista de ventanas abiertas.
        window.set_as_windows_menu_for_nsapp();

        let (tx, rx) = mpsc::channel();
        let ctx = ctx.clone();
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            let _ = tx.send(event.id);
            ctx.request_repaint();
        }));
        Ok(Self {
            _menu: menu,
            items,
            rx,
            state: None,
        })
    }

    /// Órdenes elegidas en el menú desde el último fotograma.
    pub fn commands(&self) -> Vec<Command> {
        self.rx
            .try_iter()
            .filter_map(|id| {
                self.items
                    .iter()
                    .find(|(item, _)| *item.id() == id)
                    .map(|(_, c)| *c)
            })
            .collect()
    }

    /// Activa o desactiva opciones (solo cuando algo cambia).
    pub fn sync(&mut self, state: MenuState) {
        if self.state == Some(state) {
            return;
        }
        self.state = Some(state);
        for (item, command) in &self.items {
            item.set_enabled(match command {
                Command::Open | Command::OpenFolder => state.can_open,
                Command::Export => state.can_export,
                Command::Fit | Command::OneToOne => state.has_photo,
                Command::ShowPhoto | Command::ShowBatch => true,
            });
        }
    }
}
