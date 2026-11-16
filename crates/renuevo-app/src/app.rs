//! Estado principal de la app y coordinación de los trabajos en segundo plano.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use egui::{
    Align, ColorImage, FontId, Frame, Key, KeyboardShortcut, Layout, Margin, Modifiers, Rect,
    RichText, Sense, Ui, UiBuilder, ViewportCommand, pos2, vec2,
};
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
use crate::theme::{self, semibold};
use crate::viewer::{CompareMode, Layer, PROXY_MAX_SIDE, Viewer};
use crate::widgets;
use crate::worker::{Job, Poll};

/// Área máxima (MP) de la zona visible para lanzar la vista previa automática.
const REGION_PREVIEW_MAX_MP: f32 = 2.5;
/// Espera tras el último cambio antes de lanzar la vista previa (s).
const REGION_PREVIEW_DEBOUNCE: f64 = 0.35;

/// Alto de la barra de título de macOS 27 (medido con AppKit). La barra de herramientas ocupa
/// ese mismo hueco, así que los botones de la ventana (14 pt, centrados a 16 pt del borde)
/// quedan centrados en ella.
pub const TOOLBAR_HEIGHT: f32 = 32.0;
/// Hueco para los botones de la ventana, que llegan a x = 69 pt.
const WINDOW_BUTTONS_WIDTH: i8 = 80;

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

/// Doble clic en la barra de título: lo que tenga elegido el usuario en Ajustes del Sistema
/// (Escritorio y Dock › «Al hacer doble clic en la barra de título»): ampliar (por omisión),
/// minimizar o nada.
fn title_bar_double_click(ctx: &egui::Context) {
    let action = if cfg!(target_os = "macos") {
        std::process::Command::new("defaults")
            .args(["read", "-g", "AppleActionOnDoubleClick"])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
            .unwrap_or_default()
    } else {
        String::new()
    };
    match action.as_str() {
        "Minimize" => ctx.send_viewport_cmd(ViewportCommand::Minimized(true)),
        "None" => {}
        _ => {
            let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
            ctx.send_viewport_cmd(ViewportCommand::Maximized(!maximized));
        }
    }
}

/// Segundos con una cifra decimal y coma, como se escriben en español.
fn seconds(d: Duration) -> String {
    panels::decimal(d.as_secs_f64(), 1)
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
    /// Título que lleva la ventana (para el menú Ventana y Mission Control).
    window_title: String,
    #[cfg(target_os = "macos")]
    menu: Option<crate::menu::NativeMenu>,
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
            window_title: "Renuevo".to_owned(),
            #[cfg(target_os = "macos")]
            menu: None,
        }
    }
}

impl RenuevoApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        theme::install(&cc.egui_ctx);
        // ⌘+ / ⌘− cambiarían la escala de toda la interfaz (y descolocarían la barra de título
        // respecto a los botones de la ventana); ⌘0 es «Tamaño real» de la foto.
        cc.egui_ctx.options_mut(|o| o.zoom_with_keyboard = false);
        Self::default()
    }

    /// Pone la barra de menús nativa (solo desde `main`: en los tests no hay app de AppKit).
    #[cfg(target_os = "macos")]
    pub fn with_native_menu(mut self, ctx: &egui::Context) -> Self {
        match crate::menu::NativeMenu::install(ctx) {
            Ok(menu) => self.menu = Some(menu),
            Err(e) => eprintln!("No se pudo crear la barra de menús: {e}"),
        }
        self
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
        let (w, h) = config.output_size(state.photo.image.width(), state.photo.image.height());
        if let Some(problem) = self.settings.encode.format.size_problem(w, h) {
            self.error(&problem);
            return;
        }
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
                let image = processed.as_deref().unwrap_or(&photo.image);
                let stage = if encode.uses_oxipng(image.width(), image.height(), image.color()) {
                    "Comprimiendo con oxipng y comparando cada píxel"
                } else {
                    "Codificando y comparando cada píxel"
                };
                progress.begin(stage, 1);
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
                    "Abierta en {} s: {} × {} · {}",
                    seconds(elapsed),
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
                self.success(format!("Procesada en {} s", seconds(out.elapsed)));
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
                    "Guardada y verificada en {} s: {}",
                    seconds(elapsed),
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

    fn dialog_open_folder(&mut self) {
        if self.refuse_during_batch() {
            return;
        }
        if let Some(dir) = rfd::FileDialog::new().pick_folder() {
            self.tab = Tab::Batch;
            self.batch.set_input_dir(dir);
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
            // Sin modificadores: ⌘1 es «Ver › Foto» en el menú.
            let (fit, one) = ctx.input(|i| {
                let plain = i.modifiers.is_none();
                (
                    plain && i.key_pressed(Key::F),
                    plain && i.key_pressed(Key::Num1),
                )
            });
            if fit {
                self.viewer.request_fit();
            }
            if one {
                self.viewer.one_to_one(ctx.pixels_per_point());
            }
        }
    }

    #[cfg(target_os = "macos")]
    fn handle_menu(&mut self, ctx: &egui::Context) {
        use crate::menu::{Command, MenuState};
        let Some(menu) = &mut self.menu else { return };
        let busy = self.job.is_some() || self.batch.is_running();
        menu.sync(MenuState {
            can_open: !busy,
            can_export: !busy && self.photo.is_some(),
            has_photo: self.photo.is_some(),
        });
        for command in menu.commands() {
            match command {
                Command::Open => self.dialog_open(ctx),
                Command::OpenFolder => self.dialog_open_folder(),
                Command::Export => {
                    self.tab = Tab::Photo;
                    self.dialog_export(ctx);
                }
                Command::ShowPhoto => self.tab = Tab::Photo,
                Command::ShowBatch => self.tab = Tab::Batch,
                Command::Fit => {
                    self.tab = Tab::Photo;
                    self.viewer.request_fit();
                }
                Command::OneToOne => {
                    self.tab = Tab::Photo;
                    self.viewer.one_to_one(ctx.pixels_per_point());
                }
            }
        }
    }

    #[cfg(not(target_os = "macos"))]
    fn handle_menu(&mut self, _ctx: &egui::Context) {}

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

    /// Barra de herramientas fundida con la de título: título de la foto, pestañas y «Abrir…».
    fn toolbar(&mut self, ui: &mut Ui) -> bool {
        let p = theme::palette(ui);
        let fullscreen = ui.input(|i| i.viewport().fullscreen.unwrap_or(false));
        // En pantalla completa macOS esconde los botones de la ventana.
        let inset = if cfg!(target_os = "macos") && !fullscreen {
            WINDOW_BUTTONS_WIDTH
        } else {
            12
        };
        let (title, subtitle) = match &self.photo {
            Some(state) => {
                let photo = &state.photo;
                let name = photo
                    .info
                    .path
                    .as_ref()
                    .and_then(|p| p.file_name())
                    .map_or_else(|| "Foto".to_owned(), |n| n.to_string_lossy().into_owned());
                let detail = format!(
                    "{} × {} · {}",
                    photo.width(),
                    photo.height(),
                    photo.info.format.label()
                );
                (name, Some(detail))
            }
            None => ("Renuevo".to_owned(), None),
        };
        if self.window_title != title {
            ui.ctx()
                .send_viewport_cmd(ViewportCommand::Title(title.clone()));
            self.window_title.clone_from(&title);
        }
        let can_open = self.job.is_none() && !self.batch.is_running();
        let mut open = false;
        egui::Panel::top("toolbar")
            .exact_size(TOOLBAR_HEIGHT)
            .frame(Frame::new().fill(p.toolbar).inner_margin(Margin {
                left: inset,
                right: 10,
                top: 0,
                bottom: 0,
            }))
            .show(ui, |ui| {
                let bar = ui.max_rect();
                // Arrastrar la barra mueve la ventana y el doble clic la amplía, como en macOS.
                let drag = ui.interact(
                    bar,
                    egui::Id::unique("window-drag"),
                    Sense::click_and_drag(),
                );
                let tabs = Rect::from_center_size(bar.center(), vec2(160.0, 24.0));

                // El título y su detalle, sin pasar por debajo de las pestañas: si no cabe, se
                // acorta el nombre y el detalle se queda.
                let title_area = Rect::from_min_max(bar.min, pos2(tabs.left() - 16.0, bar.max.y));
                ui.scope_builder(
                    UiBuilder::new()
                        .max_rect(title_area)
                        .layout(Layout::left_to_right(Align::Center)),
                    |ui| {
                        let subtitle_width = subtitle.as_ref().map_or(0.0, |s| {
                            ui.painter()
                                .layout_no_wrap(s.clone(), FontId::proportional(11.0), p.secondary)
                                .size()
                                .x
                                + ui.spacing().item_spacing.x
                        });
                        let name_width = (ui.available_width() - subtitle_width).max(40.0);
                        ui.scope(|ui| {
                            ui.set_max_width(name_width);
                            ui.add(
                                egui::Label::new(
                                    RichText::new(&title)
                                        .font(FontId::new(13.0, semibold()))
                                        .color(p.text),
                                )
                                .truncate(),
                            );
                        });
                        if let Some(subtitle) = &subtitle {
                            ui.add(egui::Label::new(
                                RichText::new(subtitle).small().color(p.secondary),
                            ));
                        }
                    },
                );

                let tabs_response = ui.scope_builder(
                    UiBuilder::new()
                        .max_rect(tabs)
                        .layout(Layout::left_to_right(Align::Center)),
                    |ui| {
                        widgets::segmented(
                            ui,
                            &mut self.tab,
                            &[(Tab::Photo, "Foto"), (Tab::Batch, "Lotes")],
                            true,
                        );
                    },
                );

                let open_response = ui
                    .scope_builder(
                        UiBuilder::new()
                            .max_rect(bar)
                            .layout(Layout::right_to_left(Align::Center)),
                        |ui| {
                            ui.add_enabled(can_open, egui::Button::new("Abrir…"))
                                .on_hover_text(
                                    "⌘O · o arrastra una foto o una carpeta a la ventana",
                                )
                        },
                    )
                    .inner;
                open = open_response.clicked();

                // Arrastrar la barra mueve la ventana y el doble clic hace lo que diga macOS,
                // salvo si se empieza sobre las pestañas o el botón: egui le pasa el arrastre al
                // fondo cuando el control de encima solo atiende clics.
                let on_control = ui.input(|i| i.pointer.press_origin()).is_some_and(|at| {
                    tabs_response.response.rect.contains(at) || open_response.rect.contains(at)
                });
                if drag.drag_started() && !on_control {
                    ui.ctx().send_viewport_cmd(ViewportCommand::StartDrag);
                }
                if drag.double_clicked() && !on_control {
                    title_bar_double_click(ui.ctx());
                }
            });
        open
    }

    fn side_panel(&mut self, ui: &mut Ui) -> Vec<Action> {
        let p = theme::palette(ui);
        let config = self.config();
        let current = self.after_full.as_ref().filter(|a| a.config == config);
        let processed_current = current.is_some();
        let restore_report = current.and_then(|a| a.report.as_ref());
        // Un lote en marcha ya usa la memoria y los núcleos: no se procesa otra foto a la vez.
        let busy = self.job.is_some() || self.batch.is_running();
        let color_managed = self
            .photo
            .as_ref()
            .is_some_and(|p| p.before.source().is_color_managed());
        egui::Panel::right("inspector")
            .resizable(true)
            .default_size(360.0)
            .size_range(320.0..=520.0)
            .frame(Frame::new().fill(p.sidebar).inner_margin(Margin {
                left: 12,
                right: 4,
                top: 2,
                bottom: 0,
            }))
            .show(ui, |ui| {
                // Sin encogerse al contenido: si no, el panel acaba en su ancho mínimo.
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width() - 8.0);
                        let actions = panels::side_panel(
                            ui,
                            PanelState {
                                settings: &mut self.settings,
                                photo: self.photo.as_ref().map(|p| p.photo.as_ref()),
                                busy,
                                processed_current,
                                restore_report,
                                last_export: self.last_export.as_ref(),
                                color_managed,
                            },
                        );
                        ui.add_space(12.0);
                        actions
                    })
                    .inner
            })
            .inner
    }

    fn photo_view(&mut self, ui: &mut Ui) -> bool {
        let p = theme::palette(ui);
        let can_open = self.job.is_none() && !self.batch.is_running();
        let ppp = ui.ctx().pixels_per_point();
        let config = self.config();
        let exact = config.is_exact_copy();

        if self.photo.is_some() {
            Frame::new()
                .fill(p.window)
                .inner_margin(Margin::symmetric(10, 6))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        // Mismo alto con y sin el control segmentado: la foto no salta.
                        ui.set_min_height(28.0);
                        if !exact {
                            widgets::segmented(
                                ui,
                                &mut self.viewer.compare,
                                &[
                                    (CompareMode::Split, "Comparar"),
                                    (CompareMode::Before, "Antes"),
                                    (CompareMode::After, "Después"),
                                ],
                                false,
                            );
                        }
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui
                                .button("1:1")
                                .on_hover_text("1 · ⌘0 · un píxel de la foto por píxel de pantalla")
                                .clicked()
                            {
                                self.viewer.one_to_one(ppp);
                            }
                            if ui
                                .button("Ajustar")
                                .on_hover_text("F · ⌘9 · o doble clic en la foto")
                                .clicked()
                            {
                                self.viewer.request_fit();
                            }
                        });
                    });
                });
        }

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
        viewer.ui(
            ui,
            photo.as_mut().map(|p| &mut p.before),
            after,
            hint,
            can_open,
        )
    }

    fn status_bar(&mut self, ui: &mut Ui) {
        let p = theme::palette(ui);
        if let Some(job) = &self.job {
            ui.add(egui::Spinner::new().size(12.0));
            ui.label(&job.label);
            ui.label(RichText::new(job.progress.stage()).color(p.secondary));
            ui.add(
                egui::ProgressBar::new(job.progress.fraction())
                    .desired_width(180.0)
                    .desired_height(6.0)
                    .corner_radius(3),
            );
            ui.label(RichText::new(format!("{} s", seconds(job.elapsed()))).color(p.secondary));
            if job.cancellable && ui.small_button("Cancelar").clicked() {
                job.cancel();
                self.pending_export = None;
            }
        } else if self.batch.is_running() {
            ui.add(egui::Spinner::new().size(12.0));
            ui.label("Procesando una carpeta (pestaña «Lotes»)");
            if let Some(m) = &self.message
                && m.kind == MessageKind::Error
            {
                ui.separator();
                widgets::dot_label(ui, p.red, RichText::new(&m.text).color(p.red));
            }
        } else if let Some(m) = &self.message {
            let (dot, text) = match m.kind {
                MessageKind::Info => (p.secondary, p.secondary),
                MessageKind::Success => (p.green, p.text),
                MessageKind::Error => (p.red, p.red),
            };
            widgets::dot_label(ui, dot, RichText::new(&m.text).color(text));
        } else {
            ui.label(RichText::new("Listo").color(p.secondary));
        }
    }
}

impl eframe::App for RenuevoApp {
    /// Se ejecuta también con la ventana minimizada: así «procesar y exportar» no se detiene.
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_jobs(ctx);
        // Aquí y no en `ui`: el menú también funciona con la ventana minimizada.
        self.handle_menu(ctx);
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
        let p = theme::palette(ui);

        let mut actions = Vec::new();
        if self.toolbar(ui) {
            actions.push(Action::Open);
        }
        egui::Panel::bottom("status")
            .exact_size(26.0)
            .frame(
                Frame::new()
                    .fill(p.toolbar)
                    .inner_margin(Margin::symmetric(12, 0)),
            )
            .show(ui, |ui| ui.horizontal_centered(|ui| self.status_bar(ui)));

        match self.tab {
            Tab::Photo => {
                actions.extend(self.side_panel(ui));
                let open = egui::CentralPanel::default()
                    .frame(Frame::new().fill(p.window))
                    .show(ui, |ui| self.photo_view(ui))
                    .inner;
                if open {
                    actions.push(Action::Open);
                }
            }
            Tab::Batch => {
                // También una vista previa de zona en curso (puede generar hasta 40 MP).
                let photo_busy = self.job.is_some() || self.region_job.is_some();
                egui::CentralPanel::default()
                    .frame(Frame::new().fill(p.window).inner_margin(Margin::same(20)))
                    .show(ui, |ui| self.batch.ui(ui, &self.settings, photo_busy));
            }
        }
        for action in actions {
            match action {
                Action::Open => self.dialog_open(&ctx),
                Action::Process => self.start_process(&ctx),
                Action::Export => self.dialog_export(&ctx),
            }
        }
        if self.tab == Tab::Photo {
            self.update_region_preview(&ctx);
        }
        widgets::paint_focus_ring(&ctx);

        if self.job.is_some() || self.region_job.is_some() {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
    }
}
