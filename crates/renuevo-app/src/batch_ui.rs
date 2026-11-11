//! Pestaña «Lotes»: aplicar los ajustes actuales a todas las fotos de una carpeta.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use egui::{Color32, RichText, Ui};
use renuevo_core::batch::{
    BatchJob, BatchSummary, ItemStatus, list_images, plan_outputs, run_batch,
};

use crate::panels::human_bytes;
use crate::settings::{Mode, Settings};
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
        let running = self.is_running();

        ui.heading("Procesar una carpeta");
        ui.label("Se aplican los ajustes de la pestaña «Foto» a cada imagen. Los originales nunca se modifican.");
        ui.add_space(8.0);

        egui::Grid::new("batch-grid")
            .num_columns(2)
            .spacing([12.0, 8.0])
            .show(ui, |ui| {
                ui.label("Origen");
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(!running, egui::Button::new("Elegir carpeta…"))
                        .clicked()
                        && let Some(dir) = rfd::FileDialog::new().pick_folder()
                    {
                        self.set_input_dir(dir);
                    }
                    ui.label(path_label(self.input_dir.as_deref()));
                });
                ui.end_row();

                ui.label("Destino");
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(!running, egui::Button::new("Elegir carpeta…"))
                        .clicked()
                        && let Some(dir) = rfd::FileDialog::new().pick_folder()
                    {
                        self.output_dir = Some(dir);
                    }
                    ui.label(path_label(self.output_dir.as_deref()));
                });
                ui.end_row();

                ui.label("Sufijo");
                ui.add_enabled(
                    !running,
                    egui::TextEdit::singleline(&mut self.suffix).desired_width(140.0),
                );
                ui.end_row();

                ui.label("");
                ui.add_enabled(
                    !running,
                    egui::Checkbox::new(&mut self.skip_existing, "Omitir las que ya existan"),
                );
                ui.end_row();

                ui.label("Ajustes");
                ui.label(settings_summary(settings));
                ui.end_row();
            });

        if let (Some(first), Some(out)) = (self.files.first(), self.output_dir.as_deref()) {
            let planned = plan_outputs(&self.files, out, &self.suffix, settings.encode.format);
            let example = planned.first().cloned().unwrap_or_else(|| first.clone());
            ui.label(
                RichText::new(format!("Ejemplo: {}", example.display()))
                    .small()
                    .weak(),
            );
        }

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            let can_start =
                !running && !photo_busy && !self.files.is_empty() && self.output_dir.is_some();
            if ui
                .add_enabled(
                    can_start,
                    egui::Button::new(format!("Procesar {} fotos", self.files.len())),
                )
                .clicked()
            {
                self.start(ui.ctx(), settings);
            }
            if running
                && ui.button("Cancelar").clicked()
                && let Some(job) = &self.job
            {
                job.cancel();
            }
            if photo_busy && !running {
                ui.label(RichText::new("Esperando a que termine el trabajo de la foto…").weak());
            }
        });

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
            ui.add(
                egui::ProgressBar::new(finished as f32 / total as f32)
                    .text(format!("{finished} / {total} · {}", job.progress.stage())),
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
            ui.label(RichText::new(text).strong());
        }
        if let Some(e) = &self.error {
            ui.colored_label(Color32::from_rgb(230, 90, 80), e);
        }

        ui.separator();
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                egui::Grid::new("batch-files")
                    .num_columns(2)
                    .striped(true)
                    .spacing([16.0, 4.0])
                    .show(ui, |ui| {
                        for (path, status) in self.files.iter().zip(statuses.iter()) {
                            ui.label(
                                path.file_name()
                                    .map(|n| n.to_string_lossy().into_owned())
                                    .unwrap_or_default(),
                            );
                            let (color, text) = match status {
                                ItemStatus::Pending => (Color32::GRAY, "pendiente".to_owned()),
                                ItemStatus::Running => {
                                    (Color32::from_rgb(120, 170, 255), "procesando…".to_owned())
                                }
                                ItemStatus::Done { bytes, .. } => (
                                    Color32::from_rgb(110, 200, 120),
                                    format!("verificada · {}", human_bytes(*bytes)),
                                ),
                                ItemStatus::Skipped(why) => {
                                    (Color32::from_rgb(230, 180, 60), format!("omitida: {why}"))
                                }
                                ItemStatus::Failed(why) => {
                                    (Color32::from_rgb(230, 90, 80), format!("error: {why}"))
                                }
                            };
                            ui.colored_label(color, text);
                            ui.end_row();
                        }
                    });
            });
    }
}

fn path_label(path: Option<&Path>) -> String {
    path.map_or_else(|| "—".to_owned(), |p| p.display().to_string())
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
                    "Ampliar ×{} ({})",
                    settings.upscale.factor,
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
