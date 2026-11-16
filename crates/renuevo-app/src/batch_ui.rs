//! Pestaña «Lotes»: aplicar los ajustes actuales a todas las fotos de una carpeta.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use egui::{Align, Color32, FontId, Layout, Rect, RichText, Sense, Ui, UiBuilder, pos2, vec2};
use renuevo_core::batch::{
    BatchJob, BatchSummary, ItemStatus, list_images, plan_outputs, run_batch,
};

use crate::panels::human_bytes;
use crate::settings::{Mode, Settings};
use crate::theme::{Palette, palette, semibold};
use crate::widgets::{card, form_row, hairline, note, primary_button, section, toggle_row};
use crate::worker::{Job, Poll};

pub struct BatchUi {
    input_dir: Option<PathBuf>,
    files: Vec<PathBuf>,
    output_dir: Option<PathBuf>,
    suffix: String,
    skip_existing: bool,
    statuses: Arc<Mutex<Vec<ItemStatus>>>,
    job: Option<Job<BatchSummary>>,
    summary: Option<BatchSummary>,
    error: Option<String>,
}

impl Default for BatchUi {
    fn default() -> Self {
        Self {
            input_dir: None,
            files: Vec::new(),
            output_dir: None,
            suffix: "_renuevo".to_owned(),
            skip_existing: true,
            statuses: Arc::default(),
            job: None,
            summary: None,
            error: None,
        }
    }
}

impl BatchUi {
    pub fn is_running(&self) -> bool {
        self.job.is_some()
    }

    pub fn file_count(&self) -> usize {
        self.files.len()
    }

    pub fn set_input_dir(&mut self, dir: PathBuf) {
        // Cambiar la lista con un lote en marcha mezclaría los estados de dos carpetas.
        if self.is_running() {
            return;
        }
        match list_images(&dir) {
            Ok(files) => {
                self.output_dir = Some(dir.join("renuevo"));
                *self.statuses.lock().unwrap_or_else(|e| e.into_inner()) =
                    vec![ItemStatus::Pending; files.len()];
                self.files = files;
                self.input_dir = Some(dir);
                self.summary = None;
                self.error = None;
            }
            Err(e) => self.error = Some(e.to_string()),
        }
    }

    fn start(&mut self, ctx: &egui::Context, settings: &Settings) {
        let Some(output_dir) = self.output_dir.clone() else {
            return;
        };
        if let Err(e) = std::fs::create_dir_all(&output_dir) {
            self.error = Some(format!("No se pudo crear la carpeta de destino: {e}"));
            return;
        }
        *self.statuses.lock().unwrap_or_else(|e| e.into_inner()) =
            vec![ItemStatus::Pending; self.files.len()];
        self.summary = None;
        self.error = None;
        let job = BatchJob {
            inputs: self.files.clone(),
            output_dir,
            suffix: self.suffix.clone(),
            skip_existing: self.skip_existing,
            pipeline: settings.pipeline(),
            encode: settings.encode,
        };
        let statuses = Arc::clone(&self.statuses);
        let repaint = ctx.clone();
        self.job = Some(Job::spawn(ctx, "Procesando carpeta", move |progress| {
            run_batch(&job, progress, |i, status| {
                if let Some(slot) = statuses
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .get_mut(i)
                {
                    *slot = status;
                }
                repaint.request_repaint();
            })
        }));
    }

    /// Recoge el resultado del lote si terminó (se llama también con la ventana oculta).
    pub fn poll(&mut self) {
        if let Some(job) = &self.job {
            match job.poll() {
                Poll::Done(summary) => {
                    self.summary = Some(summary);
                    self.job = None;
                }
                Poll::Crashed => {
                    self.error = Some("El proceso por lotes terminó inesperadamente".to_owned());
                    self.job = None;
                }
                Poll::Pending => {}
            }
        }
    }

    /// `photo_busy`: la pestaña «Foto» está procesando; el lote espera para no duplicar la RAM.
    pub fn ui(&mut self, ui: &mut Ui, settings: &Settings, photo_busy: bool) {
        // Una columna centrada, como una hoja de ajustes de macOS.
        let full = ui.available_rect_before_wrap();
        let width = full.width().min(720.0);
        let column = Rect::from_min_max(
            pos2(full.center().x - width / 2.0, full.top()),
            pos2(full.center().x + width / 2.0, full.bottom()),
        );
        // Toda la columna se desplaza: en una ventana baja, la lista de fotos no queda reducida
        // a una fila.
        ui.scope_builder(
            UiBuilder::new()
                .max_rect(column)
                .layout(Layout::top_down(Align::Min)),
            |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        self.column(ui, settings, photo_busy, full.height())
                    });
            },
        );
        ui.allocate_rect(full, Sense::hover());
    }

    fn column(&mut self, ui: &mut Ui, settings: &Settings, photo_busy: bool, viewport: f32) {
        // Sitio para la barra de desplazamiento, que va encima del contenido.
        ui.set_max_width(ui.available_width() - 14.0);
        let p = palette(ui);
        let running = self.is_running();

        ui.label(
            RichText::new("Procesar una carpeta")
                .font(FontId::new(20.0, semibold()))
                .color(p.text),
        );
        ui.label(
            RichText::new(
                "Se aplican los ajustes de la pestaña «Foto» a cada imagen. Los originales \
                 nunca se modifican.",
            )
            .color(p.secondary),
        );

        section(ui, "Carpetas");
        card(ui, |ui| {
            if let Some(dir) = folder_row(ui, "Origen", self.input_dir.as_deref(), !running) {
                self.set_input_dir(dir);
            }
            hairline(ui);
            if let Some(dir) = folder_row(ui, "Destino", self.output_dir.as_deref(), !running) {
                self.output_dir = Some(dir);
            }
            hairline(ui);
            form_row(ui, "Sufijo del nombre", |ui, label| {
                ui.add_enabled(
                    !running,
                    egui::TextEdit::singleline(&mut self.suffix).desired_width(140.0),
                )
                .labelled_by(label);
            });
            hairline(ui);
            ui.add_enabled_ui(!running, |ui| {
                toggle_row(ui, &mut self.skip_existing, "Omitir las que ya existan");
            });
        });

        section(ui, "Ajustes");
        card(ui, |ui| {
            ui.label(settings_summary(settings));
            note(ui, "Se cambian en la pestaña «Foto».");
            if let (Some(first), Some(out)) = (self.files.first(), self.output_dir.as_deref()) {
                // Solo el primero: planificar miles de archivos en cada fotograma se nota.
                let planned = plan_outputs(
                    std::slice::from_ref(first),
                    out,
                    &self.suffix,
                    settings.encode.format,
                );
                let example = planned.first().cloned().unwrap_or_else(|| first.clone());
                let name = example.file_name().map_or_else(
                    || example.display().to_string(),
                    |n| n.to_string_lossy().into_owned(),
                );
                ui.add(
                    egui::Label::new(
                        RichText::new(format!("Se guardarán como: {name}"))
                            .small()
                            .color(p.secondary),
                    )
                    .wrap(),
                )
                .on_hover_text(example.display().to_string());
            }
        });

        ui.add_space(10.0);
        // Una fila de botones alineada a la derecha (sin ocupar el alto que queda).
        ui.allocate_ui_with_layout(
            vec2(ui.available_width(), 30.0),
            Layout::right_to_left(Align::Center),
            |ui| {
                let can_start =
                    !running && !photo_busy && !self.files.is_empty() && self.output_dir.is_some();
                let label = format!("Procesar {} fotos", self.files.len());
                if primary_button(ui, &label, can_start, false).clicked() {
                    self.start(ui.ctx(), settings);
                }
                if running
                    && ui.button("Cancelar").clicked()
                    && let Some(job) = &self.job
                {
                    job.cancel();
                }
                if photo_busy && !running {
                    ui.label(
                        RichText::new("Esperando a que termine el trabajo de la foto…")
                            .color(p.secondary),
                    );
                }
            },
        );

        let statuses = self
            .statuses
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let finished = statuses
            .iter()
            .filter(|s| !matches!(s, ItemStatus::Pending | ItemStatus::Running))
            .count();
        if let Some(job) = &self.job {
            let total = self.files.len().max(1);
            ui.add_space(6.0);
            ui.add(
                egui::ProgressBar::new(finished as f32 / total as f32)
                    .desired_height(6.0)
                    .corner_radius(3),
            );
            note(
                ui,
                format!("{finished} / {total} · {}", job.progress.stage()),
            );
        }
        if let Some(s) = &self.summary {
            let mut text = format!(
                "Listas: {} · omitidas: {} · con error: {}",
                s.done, s.skipped, s.failed
            );
            if s.cancelled {
                text.push_str(" · cancelado");
            }
            ui.label(RichText::new(text).font(FontId::new(13.0, semibold())));
        }
        if let Some(e) = &self.error {
            ui.add(egui::Label::new(RichText::new(e).color(p.red)).selectable(true));
        }

        if self.files.is_empty() {
            return;
        }
        section(ui, &format!("Fotos ({})", self.files.len()));
        // La lista ocupa lo que queda de la ventana, y al menos unas diez filas.
        let list_height = (viewport - ui.min_rect().height() - 60.0).max(240.0);
        card(ui, |ui| {
            // Solo se dibujan las filas visibles: con miles de archivos, dibujarlas todas en
            // cada fotograma ralentiza la pestaña.
            ui.spacing_mut().item_spacing.y = 0.0;
            let row_height = ui.text_style_height(&egui::TextStyle::Body) + 8.0;
            let stripe = ui.visuals().faint_bg_color;
            egui::ScrollArea::vertical()
                .max_height(list_height)
                .auto_shrink([false, true])
                .show_rows(ui, row_height, self.files.len(), |ui, rows| {
                    let first = rows.start;
                    let files = self.files.get(rows.clone()).unwrap_or_default();
                    let states = statuses.get(rows).unwrap_or_default();
                    for (i, (path, status)) in files.iter().zip(states).enumerate() {
                        let (rect, _) = ui.allocate_exact_size(
                            vec2(ui.available_width(), row_height),
                            Sense::hover(),
                        );
                        if (first + i) % 2 == 1 {
                            ui.painter().rect_filled(rect, 4.0, stripe);
                        }
                        let name = path
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_default();
                        file_row(ui, rect, p, &name, status);
                    }
                });
        });
    }
}

/// Una fila de la lista: punto de estado, nombre (acortado si no cabe) y estado a la derecha.
/// Son etiquetas de verdad, no texto pintado, para que las lean VoiceOver y los tests.
fn file_row(ui: &mut Ui, rect: Rect, p: &Palette, name: &str, status: &ItemStatus) {
    let (dot, color, text) = status_label(p, status);
    let mut row = ui.new_child(
        UiBuilder::new()
            .max_rect(rect.shrink2(vec2(8.0, 0.0)))
            .layout(Layout::left_to_right(Align::Center)),
    );
    let (d, _) = row.allocate_exact_size(vec2(8.0, 8.0), Sense::hover());
    row.painter().circle_filled(d.center(), 4.0, dot);
    let status_font = FontId::proportional(12.0);
    let status_width = row
        .painter()
        .layout_no_wrap(text.clone(), status_font.clone(), color)
        .size()
        .x;
    let name_width = (row.available_width() - status_width - 16.0).max(40.0);
    row.scope(|ui| {
        ui.set_max_width(name_width);
        ui.add(egui::Label::new(name).truncate());
    });
    row.with_layout(Layout::right_to_left(Align::Center), |ui| {
        ui.label(RichText::new(text).font(status_font).color(color));
    });
}

/// Fila con el nombre de la carpeta y «Elegir…»; devuelve la carpeta elegida.
fn folder_row(ui: &mut Ui, label: &str, dir: Option<&Path>, enabled: bool) -> Option<PathBuf> {
    let p = palette(ui);
    let mut chosen = None;
    ui.horizontal(|ui| {
        ui.label(label);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui
                .add_enabled(enabled, egui::Button::new("Elegir…"))
                .clicked()
            {
                chosen = rfd::FileDialog::new().pick_folder();
            }
            // Como en el Finder: el nombre de la carpeta, y la ruta entera al pasar el ratón.
            let shown = ui.add(
                egui::Label::new(RichText::new(folder_name(dir)).color(p.secondary)).truncate(),
            );
            if let Some(dir) = dir {
                shown.on_hover_text(dir.display().to_string());
            }
        });
    });
    chosen
}

/// Color del punto, color del texto y texto del estado de una foto. Los estados neutros van en
/// gris secundario (el terciario no se lee bien como texto).
fn status_label(p: &Palette, status: &ItemStatus) -> (Color32, Color32, String) {
    match status {
        ItemStatus::Pending => (p.tertiary, p.secondary, "pendiente".to_owned()),
        ItemStatus::Running => (p.accent, p.secondary, "procesando…".to_owned()),
        ItemStatus::Done { bytes, .. } => (
            p.green,
            p.green,
            format!("verificada · {}", human_bytes(*bytes)),
        ),
        ItemStatus::Skipped(why) => (p.orange, p.orange, format!("omitida: {why}")),
        ItemStatus::Failed(why) => (p.red, p.red, format!("error: {why}")),
        ItemStatus::Cancelled => (p.tertiary, p.secondary, "cancelada".to_owned()),
    }
}

fn folder_name(path: Option<&Path>) -> String {
    match path {
        None => "Sin elegir".to_owned(),
        Some(p) => p.file_name().map_or_else(
            || p.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        ),
    }
}

fn settings_summary(settings: &Settings) -> String {
    let format = settings.encode.format.extension().to_uppercase();
    match settings.mode {
        Mode::ExactCopy => format!("Copia exacta · {format}"),
        Mode::Enhance => {
            let mut parts = Vec::new();
            if settings.restore_enabled {
                parts.push(
                    settings
                        .preset
                        .map_or("Restaurar (manual)".to_owned(), |p| {
                            format!("Restaurar ({})", p.label())
                        }),
                );
            }
            if settings.upscale_enabled {
                parts.push(format!(
                    "Ampliar ×{} con {}",
                    crate::panels::decimal(f64::from(settings.upscale.factor), 1)
                        .trim_end_matches(",0"),
                    settings.upscale.method.label()
                ));
            }
            if parts.is_empty() {
                parts.push("Sin cambios".to_owned());
            }
            format!("{} · {format}", parts.join(" + "))
        }
    }
}
