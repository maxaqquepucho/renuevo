//! Panel lateral: información de la foto, qué hacer con ella y cómo guardarla.

use egui::{Color32, RichText, Ui};
use renuevo_core::io::{OutputFormat, PngCompression};
use renuevo_core::restore::{Preset, RestoreReport};
use renuevo_core::upscale::UpscaleMethod;
use renuevo_core::{BitDepth, ColorSpaceInfo, ExportReport, Photo, SourceFormat};

use crate::settings::{Mode, Settings};

pub enum Action {
    Open,
    Process,
    Export,
}

pub struct PanelState<'a> {
    pub settings: &'a mut Settings,
    pub photo: Option<&'a Photo>,
    pub busy: bool,
    /// El resultado procesado corresponde a los ajustes actuales.
    pub processed_current: bool,
    pub restore_report: Option<&'a RestoreReport>,
    pub last_export: Option<&'a ExportReport>,
}

const WARN: Color32 = Color32::from_rgb(230, 180, 60);
const OK: Color32 = Color32::from_rgb(110, 200, 120);

pub fn side_panel(ui: &mut Ui, state: PanelState<'_>) -> Vec<Action> {
    let mut actions = Vec::new();
    let PanelState {
        settings,
        photo,
        busy,
        processed_current,
        restore_report,
        last_export,
    } = state;

    ui.add_space(6.0);
    if ui
        .add_enabled(!busy, egui::Button::new("Abrir foto…"))
        .on_hover_text("⌘O · También puedes arrastrar la foto a la ventana")
        .clicked()
    {
        actions.push(Action::Open);
    }
    ui.add_space(6.0);

    if let Some(photo) = photo {
        photo_info(ui, photo);
    }

    ui.separator();
    ui.heading("Qué hacer");
    ui.radio_value(&mut settings.mode, Mode::ExactCopy, "Copia exacta")
        .on_hover_text(
            "Regenera cada píxel tal cual en un formato sin pérdida y lo verifica al guardar.",
        );
    ui.radio_value(
        &mut settings.mode,
        Mode::Enhance,
        "Mejorar (restaurar y/o ampliar)",
    );

    match settings.mode {
        Mode::ExactCopy => {
            ui.label(
                RichText::new(
                    "Los píxeles no cambian. Al guardar se vuelve a leer el archivo y se compara cada píxel con el original.",
                )
                .small(),
            );
        }
        Mode::Enhance => {
            let is_jpeg = photo.is_some_and(|p| p.info.format == SourceFormat::Jpeg);
            enhance_controls(ui, settings, is_jpeg, restore_report);

            if let Some(photo) = photo {
                let cfg = settings.pipeline();
                let (w, h) = cfg.output_size(photo.width(), photo.height());
                let mp = f64::from(w) * f64::from(h) / 1e6;
                let ram = cfg.estimate_peak_bytes(photo.width(), photo.height()) as f64 / 1e9;
                let text = format!("Salida: {w} × {h} ({mp:.1} MP) · RAM ≈ {ram:.1} GB");
                if mp > settings.max_output_megapixels {
                    ui.colored_label(
                        WARN,
                        format!(
                            "{text}\nSupera el límite de {:.0} MP",
                            settings.max_output_megapixels
                        ),
                    );
                } else {
                    ui.label(RichText::new(text).small());
                }
            }

            ui.add_space(4.0);
            let label = if processed_current {
                "Procesada"
            } else {
                "Procesar"
            };
            if ui
                .add_enabled(
                    !busy && photo.is_some() && !processed_current,
                    egui::Button::new(label),
                )
                .clicked()
            {
                actions.push(Action::Process);
            }
            ui.label(
                RichText::new(
                    "Ningún algoritmo recupera detalle que ya se perdió: se reduce el ruido y los artefactos, y los bordes quedan limpios.",
                )
                .small()
                .weak(),
            );
        }
    }

    ui.separator();
    ui.heading("Guardar");
    egui::ComboBox::from_label("Formato")
        .selected_text(settings.encode.format.label())
        .show_ui(ui, |ui| {
            for format in OutputFormat::ALL {
                ui.selectable_value(&mut settings.encode.format, format, format.label());
            }
        });
    if settings.encode.format == OutputFormat::Png {
        egui::ComboBox::from_label("Compresión")
            .selected_text(png_label(settings.encode.png_compression))
            .show_ui(ui, |ui| {
                for c in [
                    PngCompression::Fast,
                    PngCompression::Balanced,
                    PngCompression::Max,
                ] {
                    ui.selectable_value(&mut settings.encode.png_compression, c, png_label(c));
                }
            });
    }
    if settings.mode == Mode::Enhance && settings.encode.format != OutputFormat::WebP {
        ui.horizontal(|ui| {
            ui.label("Profundidad:");
            ui.radio_value(&mut settings.output_depth, BitDepth::Bits16, "16 bits");
            ui.radio_value(&mut settings.output_depth, BitDepth::Bits8, "8 bits");
        });
    }
    if let Some(photo) = photo
        && settings.mode == Mode::ExactCopy
        && !settings.encode.format.can_store(photo.image.color())
    {
        ui.colored_label(
            WARN,
            format!(
                "{} no puede guardar esta foto ({:?}) sin pérdida: elige {}.",
                settings.encode.format.extension().to_uppercase(),
                photo.image.color(),
                OutputFormat::suggestion_for(photo.image.color())
            ),
        );
    }
    if photo.is_some_and(|p| p.info.format == SourceFormat::Jpeg)
        && settings.mode == Mode::ExactCopy
    {
        ui.label(
            RichText::new(
                "De JPEG a PNG el archivo crece (3-10×): es el precio de no perder nada más.",
            )
            .small()
            .weak(),
        );
    }
    ui.add_space(4.0);
    if ui
        .add_enabled(!busy && photo.is_some(), egui::Button::new("Exportar…"))
        .on_hover_text("⌘S")
        .clicked()
    {
        actions.push(Action::Export);
    }

    if let Some(report) = last_export {
        export_report(ui, report);
    }
    actions
}

fn png_label(c: PngCompression) -> &'static str {
    match c {
        PngCompression::Fast => "Rápida",
        PngCompression::Balanced => "Equilibrada",
        PngCompression::Max => "Máxima (más lenta)",
    }
}

fn photo_info(ui: &mut Ui, photo: &Photo) {
    let info = &photo.info;
    ui.separator();
    if let Some(name) = info.path.as_ref().and_then(|p| p.file_name()) {
        ui.label(RichText::new(name.to_string_lossy()).strong());
    }
    egui::Grid::new("photo-info")
        .num_columns(2)
        .spacing([8.0, 2.0])
        .show(ui, |ui| {
            ui.label("Tamaño");
            ui.label(format!(
                "{} × {} ({:.1} MP)",
                photo.width(),
                photo.height(),
                photo.megapixels()
            ));
            ui.end_row();
            ui.label("Formato");
            ui.label(format!(
                "{} · {} bits/canal · {}",
                info.format.label(),
                info.bits_per_channel,
                human_bytes(info.file_size)
            ));
            ui.end_row();
            ui.label("Color");
            let color = info.color.describe();
            match &info.color {
                ColorSpaceInfo::Discarded { .. } => {
                    ui.add(egui::Label::new(RichText::new(color).color(WARN)).wrap())
                }
                _ => ui.add(egui::Label::new(color).wrap()),
            };
            ui.end_row();
            if let Some(dpi) = info.dpi {
                ui.label("Resolución");
                ui.label(if (dpi.x - dpi.y).abs() < 0.5 {
                    format!("{:.0} ppp", dpi.x)
                } else {
                    format!("{:.0} × {:.0} ppp", dpi.x, dpi.y)
                });
                ui.end_row();
            }
        });
    for note in &info.notes {
        ui.colored_label(WARN, note);
    }
    if info.had_gps {
        ui.colored_label(WARN, "EXIF con ubicación GPS: no se copiará");
    } else if info.had_exif {
        ui.label(RichText::new("EXIF presente: no se copiará").small());
    }
    if info.orientation_applied {
        ui.label(RichText::new("Orientación aplicada a los píxeles").small());
    }
}

fn enhance_controls(
    ui: &mut Ui,
    settings: &mut Settings,
    is_jpeg: bool,
    report: Option<&RestoreReport>,
) {
    ui.add_space(4.0);
    ui.checkbox(
        &mut settings.restore_enabled,
        RichText::new("Restaurar").strong(),
    );
    ui.add_enabled_ui(settings.restore_enabled, |ui| {
        ui.horizontal(|ui| {
            for preset in Preset::ALL {
                if ui
                    .selectable_label(settings.preset == Some(preset), preset.label())
                    .clicked()
                {
                    settings.apply_preset(preset, is_jpeg);
                }
            }
            if settings.preset.is_none() {
                ui.label(RichText::new("manual").weak());
            }
        });
        let before = settings.restore;
        let r = &mut settings.restore;
        ui.add(egui::Slider::new(&mut r.deblock, 0.0..=1.0).text("Bloques JPEG"))
            .on_hover_text("Suaviza la cuadrícula de 8×8 de los JPEG muy comprimidos (solo actúa si la detecta).");
        ui.add(egui::Slider::new(&mut r.chroma_noise, 0.0..=1.0).text("Ruido de color"))
            .on_hover_text("Manchas de colores en zonas oscuras.");
        ui.add(egui::Slider::new(&mut r.luma_noise, 0.0..=1.0).text("Grano"))
            .on_hover_text("Ruido de luminancia (Non-Local Means). Se adapta al ruido estimado de la foto.");
        ui.add(egui::Slider::new(&mut r.sharpen, 0.0..=1.0).text("Enfoque"))
            .on_hover_text("Máscara de enfoque con control de halos.");
        if settings.restore != before {
            settings.preset = None;
        }
        if let Some(report) = report {
            let mut text = format!("Ruido estimado: σ ≈ {:.1} / 255", report.noise_sigma_8bit);
            if report.blocking_detected {
                text.push_str(" · bloques JPEG detectados");
            }
            ui.label(RichText::new(text).small().weak());
        }
    });

    ui.add_space(6.0);
    ui.checkbox(
        &mut settings.upscale_enabled,
        RichText::new("Ampliar").strong(),
    );
    ui.add_enabled_ui(settings.upscale_enabled, |ui| {
        let u = &mut settings.upscale;
        ui.horizontal(|ui| {
            ui.label("Factor");
            for f in [1.5f32, 2.0, 3.0, 4.0] {
                if ui
                    .selectable_label((u.factor - f).abs() < 1e-3, format!("×{f}"))
                    .clicked()
                {
                    u.factor = f;
                }
            }
        });
        egui::ComboBox::from_label("Método")
            .selected_text(u.method.label())
            .show_ui(ui, |ui| {
                for m in UpscaleMethod::ALL {
                    ui.selectable_value(&mut u.method, m, m.label());
                }
            });
        ui.add(egui::Slider::new(&mut u.sharpen, 0.0..=1.0).text("Enfoque (RCAS)"))
            .on_hover_text("Enfoque adaptativo al contraste: recupera nitidez sin halos.");
    });
}

fn export_report(ui: &mut Ui, report: &ExportReport) {
    ui.add_space(6.0);
    let v = &report.verify;
    let file = report
        .path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    egui::Frame::group(ui.style()).show(ui, |ui| {
        if v.identical {
            ui.colored_label(
                OK,
                format!("Verificado: {} píxeles idénticos", thousands(v.pixels)),
            );
        }
        ui.label(RichText::new(format!("{file} · {}", human_bytes(report.bytes_written))).small());
        ui.label(
            RichText::new(format!(
                "Huella {} · {} · {}",
                v.fingerprint,
                match (v.icc_present, v.icc_preserved) {
                    (true, true) => "perfil de color conservado",
                    (true, false) => "perfil de color distinto",
                    (false, _) => "sin perfil (sRGB)",
                },
                if v.exif_present {
                    "con EXIF"
                } else {
                    "sin EXIF"
                }
            ))
            .small()
            .weak(),
        );
    });
}

pub fn human_bytes(bytes: u64) -> String {
    let b = bytes as f64;
    if b >= 1e9 {
        format!("{:.2} GB", b / 1e9)
    } else if b >= 1e6 {
        format!("{:.1} MB", b / 1e6)
    } else if b >= 1e3 {
        format!("{:.0} KB", b / 1e3)
    } else {
        format!("{bytes} B")
    }
}

pub fn thousands(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push('.');
        }
        out.push(c);
    }
    out
}
