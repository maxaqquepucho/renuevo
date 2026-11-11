//! App de escritorio de renuevo (eframe/egui 0.37).
//!
//! La lógica de imagen vive en `renuevo-core`; aquí solo hay interfaz y coordinación de
//! trabajos en segundo plano.

mod app;
mod batch_ui;
mod display;
mod panels;
mod settings;
mod viewer;
mod worker;

pub use app::RenuevoApp;
pub use settings::{Mode, Settings};
