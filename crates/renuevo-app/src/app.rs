//! Estado principal de la app y coordinación de los trabajos en segundo plano.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use egui::{Color32, ColorImage, Key, KeyboardShortcut, Modifiers, Rect, RichText, Ui, pos2, vec2};
use image::DynamicImage;
use renuevo_core::batch::{SUPPORTED_EXTENSIONS, is_supported};
use renuevo_core::restore::{NoiseLevels, RestoreReport, estimate_noise};
use renuevo_core::{
    ExportReport, Photo, PipelineConfig, SourceFormat, export, io, process, process_region,
};

use crate::batch_ui::BatchUi;
use crate::display::{DisplaySource, SharedImage};
use crate::panels::{self, Action, PanelState};
use crate::settings::Settings;
use crate::viewer::{CompareMode, Layer, PROXY_MAX_SIDE, Viewer};
use crate::worker::{Job, Poll};

/// Área máxima (MP) de la zona visible para lanzar la vista previa automática.
const REGION_PREVIEW_MAX_MP: f32 = 2.5;
/// Espera tras el último cambio antes de lanzar la vista previa (s).
const REGION_PREVIEW_DEBOUNCE: f64 = 0.35;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Photo,
    Batch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MessageKind {
    Info,
    Success,
    Error,
}

struct Message {
    kind: MessageKind,
    text: String,
}

struct Loaded {
    photo: Arc<Photo>,
    noise: NoiseLevels,
    display: DisplaySource,
    proxy: ColorImage,
}

struct ProcessedOut {
    image: Arc<DynamicImage>,
    display: DisplaySource,
    proxy: ColorImage,
    config: PipelineConfig,
    report: Option<RestoreReport>,
    elapsed: Duration,
}

struct RegionOut {
    display: DisplaySource,
    proxy: ColorImage,
    config: PipelineConfig,
    region: Rect,
}

enum JobResult {
    Loaded(Result<Loaded, String>),
    Processed(Result<ProcessedOut, String>),
    Exported(Result<ExportReport, String>),
}

struct PhotoState {
    photo: Arc<Photo>,
    /// Ruido medido una vez sobre la foto completa (lo usan la vista previa y el procesado).
    noise: NoiseLevels,
    before: Layer,
}

struct AfterState {
    layer: Layer,
    config: PipelineConfig,
    /// Solo el resultado completo se puede exportar.
    image: Option<Arc<DynamicImage>>,
    report: Option<RestoreReport>,
}

struct RegionJob {
    job: Job<Result<RegionOut, String>>,
    /// Foto para la que se calcula: si entretanto se abre otra, el resultado se descarta.
    photo: Arc<Photo>,
    config: PipelineConfig,
    key: [u32; 4],
}

/// Lado máximo de la miniatura: el del visor, sin pasar del límite de texturas de la GPU.
fn proxy_side(ctx: &egui::Context) -> u32 {
    let gpu = u32::try_from(ctx.input(|i| i.max_texture_side)).unwrap_or(u32::MAX);
    PROXY_MAX_SIDE.min(gpu)
}

const BATCH_RUNNING: &str =
    "Hay un lote en marcha: espera a que termine o cancélalo en la pestaña «Lotes».";

pub struct RenuevoApp {
    tab: Tab,
    settings: Settings,
    photo: Option<PhotoState>,
    after_full: Option<AfterState>,
    after_region: Option<AfterState>,
    viewer: Viewer,
    job: Option<Job<JobResult>>,
    region_job: Option<RegionJob>,
    /// Vista previa deseada (ajustes, zona) y desde cuándo no cambia.
    region_wanted: Option<(PipelineConfig, [u32; 4], f64)>,
    /// Última vista previa que falló: no se reintenta en bucle.
    region_failed: Option<(PipelineConfig, [u32; 4])>,
    /// Vistas previas lanzadas (para detectar bucles en los tests).
    region_runs: u64,
    pending_export: Option<PathBuf>,
    initial_file: Option<PathBuf>,
    last_export: Option<ExportReport>,
    message: Option<Message>,
    batch: BatchUi,
}

impl Default for RenuevoApp {
    fn default() -> Self {
        Self {
            tab: Tab::Photo,
            settings: Settings::default(),
            photo: None,
            after_full: None,
            after_region: None,
            viewer: Viewer::default(),
            job: None,
            region_job: None,
            region_wanted: None,
            region_failed: None,
            region_runs: 0,
            pending_export: None,
            initial_file: None,
            last_export: None,
            message: None,
            batch: BatchUi::default(),
        }
    }
}

impl RenuevoApp {
    pub fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        Self::default()
    }

    /// Abre `path` en cuanto se muestre la ventana (p. ej. `renuevo foto.heic`).
    pub fn with_initial_file(mut self, path: Option<PathBuf>) -> Self {
        self.initial_file = path;
        self
    }

    /// Configuración del procesado: los ajustes más el ruido medido en la foto abierta.
    fn config(&self) -> PipelineConfig {
        let mut config = self.settings.pipeline();
        config.noise = self.photo.as_ref().map(|p| p.noise);
        config
    }

    // ---- Accesos para tests y depuración ----

    pub fn is_busy(&self) -> bool {
        self.job.is_some() || self.region_job.is_some() || self.batch.is_running()
    }

    pub fn has_photo(&self) -> bool {
        self.photo.is_some()
    }

    pub fn settings_mut(&mut self) -> &mut Settings {
        &mut self.settings
    }

    pub fn last_export(&self) -> Option<&ExportReport> {
        self.last_export.as_ref()
    }

    pub fn has_current_result(&self) -> bool {
        let config = self.config();
        self.after_full.as_ref().is_some_and(|a| a.config == config)
    }

    /// Hay una vista previa de zona calculada con los ajustes actuales.
    pub fn has_region_preview(&self) -> bool {
        let config = self.config();
        self.after_region
            .as_ref()
            .is_some_and(|a| a.config == config)
    }

    /// Número de vistas previas de zona lanzadas desde que se abrió la app.
    pub fn region_preview_runs(&self) -> u64 {
        self.region_runs
    }

    pub fn batch_running(&self) -> bool {
        self.batch.is_running()
    }

    pub fn batch_file_count(&self) -> usize {
        self.batch.file_count()
    }

    pub fn message_text(&self) -> Option<&str> {
        self.message.as_ref().map(|m| m.text.as_str())
    }

    // ---- Trabajos ----

    pub fn open_path(&mut self, ctx: &egui::Context, path: PathBuf) {
        if self.job.is_some() || self.refuse_during_batch() {
            return;
        }
        let proxy_side = proxy_side(ctx);
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.job = Some(Job::spawn(
            ctx,
            format!("Abriendo {name}"),
            move |progress| {
                progress.begin("Leyendo y decodificando", 1);
                JobResult::Loaded(io::open(&path).map_err(|e| e.to_string()).map(|photo| {
                    let photo = Arc::new(photo);
                    progress.begin("Midiendo el ruido", 1);
                    let noise = estimate_noise(&photo.image);
                    let display = DisplaySource::new(
                        SharedImage::Photo(Arc::clone(&photo)),
                        photo.icc.as_deref(),
                    );
                    let proxy = display.proxy(proxy_side);
                    Loaded {
                        photo,
                        noise,
                        display,
                        proxy,
                    }
                }))
            },
        ));
    }

    /// Procesa la foto completa con los ajustes actuales.
    pub fn start_process(&mut self, ctx: &egui::Context) {
        if self.photo.is_none() || self.job.is_some() || self.refuse_during_batch() {
            return;
        }
        let Some(state) = &self.photo else { return };
        if let Some(r) = &self.region_job {
            r.job.cancel();
        }
        let proxy_side = proxy_side(ctx);
        let photo = Arc::clone(&state.photo);
        let config = self.config();
        self.job = Some(Job::spawn(ctx, "Procesando", move |progress| {
            JobResult::Processed(
                process(&photo.image, &config, progress)
                    .map_err(|e| e.to_string())
                    .map(|p| {
                        progress.begin("Preparando la vista", 1);
                        let image = Arc::new(p.image);
                        let display = DisplaySource::new(
                            SharedImage::Image(Arc::clone(&image)),
                            photo.icc.as_deref(),
                        );
                        let proxy = display.proxy(proxy_side);
                        ProcessedOut {
                            image,
                            display,
                            proxy,
                            config,
                            report: p.restore_report,
                            elapsed: p.elapsed,
                        }
                    }),
            )
        }));
    }

    /// Exporta a `path`. Si hace falta procesar antes, procesa y exporta al terminar.
    pub fn start_export(&mut self, ctx: &egui::Context, path: PathBuf) {
        if self.photo.is_none() || self.job.is_some() || self.refuse_during_batch() {
            return;
        }
        let Some(state) = &self.photo else { return };
        if state
            .photo
            .info
            .path
            .as_deref()
            .is_some_and(|original| renuevo_core::io::same_file(original, &path))
        {
            self.error("No se puede sobrescribir la foto original: elige otro nombre.");
            return;
        }
        let config = self.config();
        let encode = renuevo_core::io::EncodeOptions {
            dpi: config.output_dpi(state.photo.info.dpi),
            ..self.settings.encode
        };
        let photo = Arc::clone(&state.photo);
        let processed = if config.is_exact_copy() {
            None
        } else {
            match self
                .after_full
                .as_ref()
                .filter(|a| a.config == config)
                .and_then(|a| a.image.clone())
            {
                Some(image) => Some(image),
                None => {
                    self.pending_export = Some(path);
                    self.start_process(ctx);
                    return;
                }
            }
        };
        self.job = Some(
            Job::spawn(ctx, "Guardando y verificando", move |progress| {
                progress.begin("Codificando y comparando cada píxel", 1);
                let image = processed.as_deref().unwrap_or(&photo.image);
                JobResult::Exported(
                    export(image, photo.icc.as_deref(), &path, &encode).map_err(|e| e.to_string()),
                )
            })
            .not_cancellable(),
        );
    }

    fn poll_jobs(&mut self, ctx: &egui::Context) {
        if let Some(job) = &self.job {
            match job.poll() {
                Poll::Pending => {}
                Poll::Crashed => {
                    self.job = None;
                    self.pending_export = None;
                    self.error("El trabajo terminó inesperadamente.");
                }
                Poll::Done(result) => {
                    let elapsed = job.elapsed();
                    self.job = None;
                    self.on_job_done(ctx, result, elapsed);
                }
            }
        }
        if let Some(region) = &self.region_job {
            let cancelled = renuevo_core::Error::Cancelled.to_string();
            match region.job.poll() {
                Poll::Pending => {}
                Poll::Crashed => {
                    self.region_failed = Some((region.config, region.key));
                    self.region_job = None;
                }
                Poll::Done(result) => {
                    let for_current_photo = self
                        .photo
                        .as_ref()
                        .is_some_and(|p| Arc::ptr_eq(&p.photo, &region.photo));
                    if matches!(&result, Err(e) if *e != cancelled) {
                        self.region_failed = Some((region.config, region.key));
                    }
                    self.region_job = None;
                    // Errores y resultados de una foto anterior se descartan.
                    if let Ok(out) = result
                        && for_current_photo
                    {
                        let layer =
                            Layer::new(ctx, "region", Arc::new(out.display), out.proxy, out.region);
                        self.after_region = Some(AfterState {
                            layer,
                            config: out.config,
                            image: None,
                            report: None,
                        });
                    }
                }
            }
        }
    }

    fn on_job_done(&mut self, ctx: &egui::Context, result: JobResult, elapsed: Duration) {
        let cancelled = renuevo_core::Error::Cancelled.to_string();
        match result {
            JobResult::Loaded(Ok(loaded)) => {
                let photo = loaded.photo;
                let size = vec2(photo.width() as f32, photo.height() as f32);
                let region = Rect::from_min_size(pos2(0.0, 0.0), size);
                let before = Layer::new(
                    ctx,
                    "before",
                    Arc::new(loaded.display),
                    loaded.proxy,
                    region,
                );
                self.settings
                    .adapt_to_source(photo.info.format == SourceFormat::Jpeg);
                let text = format!(
                    "Abierta en {:.1} s: {} × {} · {}",
                    elapsed.as_secs_f32(),
                    photo.width(),
                    photo.height(),
                    photo.info.format.label()
                );
                if let Some(running) = self.region_job.take() {
                    running.job.cancel();
                }
                self.photo = Some(PhotoState {
                    photo,
                    noise: loaded.noise,
                    before,
                });
                self.after_full = None;
                self.after_region = None;
                self.region_wanted = None;
                self.region_failed = None;
                self.last_export = None;
                self.viewer.reset(size);
                self.info(text);
            }
            JobResult::Loaded(Err(e)) => self.error(format!("No se pudo abrir: {e}")),
            JobResult::Processed(Ok(out)) => {
                let Some(state) = &self.photo else { return };
                let region = Rect::from_min_size(
                    pos2(0.0, 0.0),
                    vec2(state.photo.width() as f32, state.photo.height() as f32),
                );
                let layer = Layer::new(ctx, "after", Arc::new(out.display), out.proxy, region);
                self.after_full = Some(AfterState {
                    layer,
                    config: out.config,
                    image: Some(out.image),
                    report: out.report,
                });
                self.after_region = None;
                self.success(format!("Procesada en {:.1} s", out.elapsed.as_secs_f32()));
                if let Some(path) = self.pending_export.take() {
                    self.start_export(ctx, path);
                }
            }
            JobResult::Processed(Err(e)) => {
                self.pending_export = None;
                if e == cancelled {
                    self.info("Procesado cancelado.");
                } else {
                    self.error(format!("No se pudo procesar: {e}"));
                }
            }
            JobResult::Exported(Ok(report)) => {
                self.success(format!(
                    "Guardada y verificada en {:.1} s: {}",
                    elapsed.as_secs_f32(),
                    report.path.display()
                ));
                self.last_export = Some(report);
            }
            JobResult::Exported(Err(e)) => self.error(format!("No se guardó: {e}")),
        }
    }

    /// Vista previa automática de la zona visible cuando hay zoom y los ajustes cambian.
    fn update_region_preview(&mut self, ctx: &egui::Context) {
        let config = self.config();
        let Some(state) = &self.photo else { return };
        let full_current = self.after_full.as_ref().is_some_and(|a| a.config == config);
        let Some(visible) = self.viewer.visible_region() else {
            return;
        };
        if config.is_exact_copy()
            || self.job.is_some()
            || self.batch.is_running()
            || full_current
            || visible.area() / 1e6 > REGION_PREVIEW_MAX_MP
        {
            self.region_wanted = None;
            return;
        }
        if self
            .after_region
            .as_ref()
            .is_some_and(|a| a.config == config && a.layer.region().contains_rect(visible))
        {
            self.region_wanted = None;
            return;
        }
        // Redondear hacia fuera los dos extremos: así la zona pedida siempre contiene la visible
        // (con «floor(min) + ceil(ancho)» podía quedarse corta y relanzarse sin fin).
        let (x0, y0) = (visible.min.x.floor(), visible.min.y.floor());
        let (x1, y1) = (visible.max.x.ceil(), visible.max.y.ceil());
        let key = [x0 as u32, y0 as u32, (x1 - x0) as u32, (y1 - y0) as u32];
        if self.region_failed == Some((config, key)) {
            return;
        }
        if let Some(running) = &self.region_job {
            if running.config == config && running.key == key {
                return; // ya se está calculando justo esto
            }
            running.job.cancel();
        }
        let now = ctx.input(|i| i.time);
        match &self.region_wanted {
            Some((c, k, _)) if *c == config && *k == key => {}
            _ => self.region_wanted = Some((config, key, now)),
        }
        let since = self.region_wanted.map_or(now, |(_, _, t)| t);
        if self.region_job.is_none()
            && now - since >= REGION_PREVIEW_DEBOUNCE
            && self.viewer.is_idle(now)
        {
            self.region_wanted = None;
            let photo = Arc::clone(&state.photo);
            let region = Rect::from_min_size(
                pos2(key[0] as f32, key[1] as f32),
                vec2(key[2] as f32, key[3] as f32),
            );
            let proxy_side = proxy_side(ctx);
            let job = Job::spawn(ctx, "Vista previa de la zona", move |progress| {
                process_region(
                    &photo.image,
                    &config,
                    (key[0], key[1], key[2], key[3]),
                    progress,
                )
                .map_err(|e| e.to_string())
                .map(|img| {
                    let display =
                        DisplaySource::new(SharedImage::Image(Arc::new(img)), photo.icc.as_deref());
                    let proxy = display.proxy(proxy_side);
                    RegionOut {
                        display,
                        proxy,
                        config,
                        region,
                    }
                })
            });
            self.region_runs += 1;
            self.region_job = Some(RegionJob {
                job,
                photo: Arc::clone(&state.photo),
                config,
                key,
            });
        } else {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
    }

    // ---- Diálogos y entrada ----

    fn dialog_open(&mut self, ctx: &egui::Context) {
        if self.job.is_some() || self.refuse_during_batch() {
            return;
        }
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("Fotos", SUPPORTED_EXTENSIONS)
            .pick_file()
        {
            self.tab = Tab::Photo;
            self.open_path(ctx, path);
        }
    }

    fn dialog_export(&mut self, ctx: &egui::Context) {
        if self.photo.is_none() || self.job.is_some() || self.refuse_during_batch() {
            return;
        }
        let Some(state) = &self.photo else { return };
        let info = &state.photo.info;
        let ext = self.settings.encode.format.extension();
        let stem = info
            .path
            .as_ref()
            .and_then(|p| p.file_stem())
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "foto".to_owned());
        let mut dialog = rfd::FileDialog::new()
            .set_file_name(format!("{stem}_renuevo.{ext}"))
            .add_filter(ext.to_uppercase(), &[ext]);
        if let Some(dir) = info.path.as_ref().and_then(|p| p.parent()) {
            dialog = dialog.set_directory(dir);
        }
        if let Some(mut path) = dialog.save_file() {
            if path.extension().is_none() {
                path.set_extension(ext);
            }
            self.start_export(ctx, path);
        }
    }

    fn handle_input(&mut self, ctx: &egui::Context) {
        if let Some(path) = self.initial_file.take() {
            self.open_path(ctx, path);
        }
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .collect()
        });
        if let Some(path) = dropped.into_iter().next()
            && !self.refuse_during_batch()
        {
            if path.is_dir() {
                self.tab = Tab::Batch;
                self.batch.set_input_dir(path);
            } else if is_supported(&path) {
                self.tab = Tab::Photo;
                self.open_path(ctx, path);
            } else {
                self.error("Ese archivo no es una foto compatible (JPEG, PNG, WebP, TIFF o HEIC).");
            }
        }

        let open = ctx
            .input_mut(|i| i.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, Key::O)));
        let save = ctx
            .input_mut(|i| i.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, Key::S)));
        if open {
            self.dialog_open(ctx);
        }
        if save && self.tab == Tab::Photo {
            self.dialog_export(ctx);
        }
        let typing = ctx.memory(|m| m.focused().is_some());
        if !typing && self.tab == Tab::Photo {
            let (fit, one) = ctx.input(|i| (i.key_pressed(Key::F), i.key_pressed(Key::Num1)));
            if fit {
                self.viewer.request_fit();
            }
            if one {
                self.viewer.one_to_one(ctx.pixels_per_point());
            }
        }
    }

    // ---- Mensajes ----

    fn set_message(&mut self, kind: MessageKind, text: impl Into<String>) {
        self.message = Some(Message {
            kind,
            text: text.into(),
        });
    }

    fn info(&mut self, text: impl Into<String>) {
        self.set_message(MessageKind::Info, text);
    }

    fn success(&mut self, text: impl Into<String>) {
        self.set_message(MessageKind::Success, text);
    }

    fn error(&mut self, text: impl Into<String>) {
        self.set_message(MessageKind::Error, text);
    }

    /// Mientras corre un lote no se abre ni se procesa otra foto (memoria y núcleos ocupados).
    fn refuse_during_batch(&mut self) -> bool {
        let running = self.batch.is_running();
        if running {
            self.error(BATCH_RUNNING);
        }
        running
    }

    // ---- Interfaz ----

    fn side_panel(&mut self, ui: &mut Ui) -> Vec<Action> {
        let config = self.config();
        let current = self.after_full.as_ref().filter(|a| a.config == config);
        let processed_current = current.is_some();
        let restore_report = current.and_then(|a| a.report.as_ref());
        // Un lote en marcha ya usa la memoria y los núcleos: no se procesa otra foto a la vez.
        let busy = self.job.is_some() || self.batch.is_running();
        egui::Panel::left("controls")
            .resizable(true)
            .default_size(340.0)
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .show(ui, |ui| {
                        panels::side_panel(
                            ui,
                            PanelState {
                                settings: &mut self.settings,
                                photo: self.photo.as_ref().map(|p| p.photo.as_ref()),
                                busy,
                                processed_current,
                                restore_report,
                                last_export: self.last_export.as_ref(),
                            },
                        )
                    })
                    .inner
            })
            .inner
    }

    fn photo_view(&mut self, ui: &mut Ui) {
        let ppp = ui.ctx().pixels_per_point();
        let config = self.config();
        let exact = config.is_exact_copy();

        ui.horizontal(|ui| {
            if ui.button("Ajustar").on_hover_text("F · o doble clic en la foto").clicked() {
                self.viewer.request_fit();
            }
            if ui.button("1:1").on_hover_text("1 · un píxel de la foto por píxel de pantalla").clicked() {
                self.viewer.one_to_one(ppp);
            }
            if !exact {
                ui.separator();
                ui.selectable_value(&mut self.viewer.compare, CompareMode::Split, "Comparar");
                ui.selectable_value(&mut self.viewer.compare, CompareMode::Before, "Antes");
                ui.selectable_value(&mut self.viewer.compare, CompareMode::After, "Después");
            }
            if self
                .photo
                .as_ref()
                .is_some_and(|p| p.before.source().is_color_managed())
            {
                ui.separator();
                ui.label(RichText::new("Vista con gestión de color").small().weak())
                    .on_hover_text("La pantalla muestra la foto convertida desde su perfil ICC. Los archivos guardados conservan el perfil original.");
            }
        });

        let Self {
            viewer,
            photo,
            after_full,
            after_region,
            region_job,
            ..
        } = self;
        let full_ok = after_full.as_ref().is_some_and(|a| a.config == config);
        let region_ok = !full_ok && after_region.as_ref().is_some_and(|a| a.config == config);
        let after = if exact {
            None
        } else if full_ok {
            after_full.as_mut().map(|a| &mut a.layer)
        } else if region_ok {
            after_region.as_mut().map(|a| &mut a.layer)
        } else {
            None
        };
        let hint = match (photo.is_some(), exact, full_ok, region_ok) {
            (false, ..) => None,
            (true, true, ..) => Some("Copia exacta: el resultado es idéntico píxel a píxel"),
            (true, false, true, _) => None,
            (true, false, false, true) => {
                Some("Vista previa de la zona visible · «Procesar» para la foto completa")
            }
            (true, false, false, false) if region_job.is_some() => {
                Some("Calculando vista previa de la zona…")
            }
            (true, false, false, false) => {
                Some("Pulsa «Procesar», o acerca el zoom para previsualizar una zona")
            }
        };
        viewer.ui(ui, photo.as_mut().map(|p| &mut p.before), after, hint);
    }

    fn status_bar(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            if let Some(job) = &self.job {
                ui.spinner();
                ui.label(&job.label);
                ui.add(
                    egui::ProgressBar::new(job.progress.fraction())
                        .desired_width(280.0)
                        .text(job.progress.stage()),
                );
                ui.label(format!("{:.1} s", job.elapsed().as_secs_f32()));
                if job.cancellable && ui.button("Cancelar").clicked() {
                    job.cancel();
                    self.pending_export = None;
                }
            } else if self.batch.is_running() {
                ui.spinner();
                ui.label("Procesando una carpeta (pestaña «Lotes»)");
                if let Some(m) = &self.message
                    && m.kind == MessageKind::Error
                {
                    ui.separator();
                    ui.colored_label(Color32::from_rgb(230, 90, 80), &m.text);
                }
            } else if let Some(m) = &self.message {
                let color = match m.kind {
                    MessageKind::Info => ui.visuals().text_color(),
                    MessageKind::Success => Color32::from_rgb(110, 200, 120),
                    MessageKind::Error => Color32::from_rgb(230, 90, 80),
                };
                ui.colored_label(color, &m.text);
            } else {
                ui.label(RichText::new("Listo").weak());
            }
        });
    }
}

impl eframe::App for RenuevoApp {
    /// Se ejecuta también con la ventana minimizada: así «procesar y exportar» no se detiene.
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_jobs(ctx);
        let batch_was_running = self.batch.is_running();
        self.batch.poll();
        // El aviso de «hay un lote en marcha» deja de ser verdad cuando el lote termina.
        if batch_was_running
            && !self.batch.is_running()
            && self
                .message
                .as_ref()
                .is_some_and(|m| m.text == BATCH_RUNNING)
        {
            self.message = None;
        }
        if self.is_busy() {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
    }

    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.handle_input(&ctx);

        egui::Panel::top("top").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.heading("Renuevo");
                ui.separator();
                ui.selectable_value(&mut self.tab, Tab::Photo, "Foto");
                ui.selectable_value(&mut self.tab, Tab::Batch, "Lotes");
            });
        });
        egui::Panel::bottom("status").show(ui, |ui| self.status_bar(ui));

        match self.tab {
            Tab::Photo => {
                let actions = self.side_panel(ui);
                egui::CentralPanel::default().show(ui, |ui| self.photo_view(ui));
                for action in actions {
                    match action {
                        Action::Open => self.dialog_open(&ctx),
                        Action::Process => self.start_process(&ctx),
                        Action::Export => self.dialog_export(&ctx),
                    }
                }
                self.update_region_preview(&ctx);
            }
            Tab::Batch => {
                // También una vista previa de zona en curso (puede generar hasta 40 MP).
                let photo_busy = self.job.is_some() || self.region_job.is_some();
                egui::CentralPanel::default()
                    .show(ui, |ui| self.batch.ui(ui, &self.settings, photo_busy));
            }
        }

        if self.job.is_some() || self.region_job.is_some() {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
    }
}
