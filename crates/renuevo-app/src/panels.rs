//! Panel de ajustes: información de la foto, qué hacer con ella y cómo guardarla, en grupos
//! como los de Ajustes del Sistema.

use egui::{FontId, RichText, Ui};
use renuevo_core::io::{OutputFormat, PngCompression};
use renuevo_core::restore::{Preset, RestoreReport};
use renuevo_core::upscale::UpscaleMethod;
use renuevo_core::{BitDepth, ColorSpaceInfo, ExportReport, Photo, SourceFormat};

use crate::settings::{Mode, Settings};
use crate::theme::{palette, semibold};
use crate::widgets::{
    card, form_row, hairline, note, popup, primary_button, section, segmented, slider_row,
    toggle_row, warning,
};

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
    /// La vista convierte el perfil de color de la foto para la pantalla.
    pub color_managed: bool,
}

pub fn side_panel(ui: &mut Ui, state: PanelState<'_>) -> Vec<Action> {
    let mut actions = Vec::new();
    let PanelState {
        settings,
        photo,
        busy,
        processed_current,
        restore_report,
        last_export,
        color_managed,
    } = state;

    if let Some(photo) = photo {
        section(ui, "Foto");
        card(ui, |ui| photo_info(ui, photo, color_managed));
    }

    section(ui, "Qué hacer");
    card(ui, |ui| {
        segmented(
            ui,
            &mut settings.mode,
            &[
                (Mode::ExactCopy, "Copia exacta"),
                (Mode::Enhance, "Mejorar"),
            ],
            true,
        );
        match settings.mode {
            Mode::ExactCopy => note(
                ui,
                "Los píxeles no cambian. Al guardar se vuelve a leer el archivo y se compara \
                 cada píxel con el original.",
            ),
            Mode::Enhance => {
                let is_jpeg = photo.is_some_and(|p| p.info.format == SourceFormat::Jpeg);
                enhance_controls(ui, settings, is_jpeg, restore_report);
                hairline(ui);
                if let Some(photo) = photo {
                    output_estimate(ui, settings, photo);
                }
                ui.horizontal(|ui| {
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
                        .on_hover_text("Procesa la foto completa con estos ajustes")
                        .clicked()
                    {
                        actions.push(Action::Process);
                    }
                });
                note(
                    ui,
                    "Ningún algoritmo recupera detalle que ya se perdió: se reduce el ruido y \
                     los artefactos, y los bordes quedan limpios.",
                );
            }
        }
    });

    section(ui, "Guardar");
    card(ui, |ui| {
        save_controls(ui, settings, photo);
        ui.add_space(4.0);
        if primary_button(ui, "Exportar…", !busy && photo.is_some(), true)
            .on_hover_text("⌘S")
            .clicked()
        {
            actions.push(Action::Export);
        }
    });

    if let Some(report) = last_export {
        ui.add_space(8.0);
        card(ui, |ui| export_report(ui, report));
    }
    actions
}

fn png_label(c: PngCompression) -> &'static str {
    match c {
        PngCompression::Fast => "Rápida",
        PngCompression::Balanced => "Equilibrada",
        PngCompression::Max => "Máxima (oxipng, más lenta)",
    }
}

fn png_short_label(c: PngCompression) -> &'static str {
    match c {
        PngCompression::Fast => "Rápida",
        PngCompression::Balanced => "Equilibrada",
        PngCompression::Max => "Máxima",
    }
}

fn photo_info(ui: &mut Ui, photo: &Photo, color_managed: bool) {
    let p = palette(ui);
    let info = &photo.info;
    if let Some(name) = info.path.as_ref().and_then(|p| p.file_name()) {
        ui.add(
            egui::Label::new(
                RichText::new(name.to_string_lossy())
                    .font(FontId::new(13.0, semibold()))
                    .color(p.text),
            )
            .truncate(),
        );
    }
    egui::Grid::new("photo-info")
        .num_columns(2)
        .spacing([10.0, 3.0])
        .show(ui, |ui| {
            let row = |ui: &mut Ui, key: &str, value: RichText| {
                ui.label(RichText::new(key).color(p.secondary));
                ui.add(egui::Label::new(value).wrap());
                ui.end_row();
            };
            row(
                ui,
                "Tamaño",
                RichText::new(format!(
                    "{} × {} ({} MP)",
                    photo.width(),
                    photo.height(),
                    decimal(photo.megapixels(), 1)
                )),
            );
            row(
                ui,
                "Formato",
                RichText::new(format!(
                    "{} · {} bits · {}",
                    info.format.label(),
                    info.bits_per_channel,
                    human_bytes(info.file_size)
                )),
            );
            let color = RichText::new(info.color.describe());
            row(
                ui,
                "Color",
                match &info.color {
                    ColorSpaceInfo::Discarded { .. } => color.color(p.orange),
                    _ => color,
                },
            );
            if let Some(dpi) = info.dpi {
                row(
                    ui,
                    "Resolución",
                    RichText::new(if (dpi.x - dpi.y).abs() < 0.5 {
                        format!("{:.0} ppp", dpi.x)
                    } else {
                        format!("{:.0} × {:.0} ppp", dpi.x, dpi.y)
                    }),
                );
            }
        });
    for n in &info.notes {
        warning(ui, n);
    }
    if info.had_gps {
        warning(ui, "EXIF con ubicación GPS: no se copiará");
    } else if info.had_exif {
        note(ui, "EXIF presente: no se copiará");
    }
    if info.orientation_applied {
        note(ui, "Orientación aplicada a los píxeles");
    }
    if color_managed {
        note(
            ui,
            "La pantalla la muestra convertida desde su perfil de color; los archivos guardados \
             conservan el perfil original.",
        );
    }
    note(
        ui,
        "La copia lleva solo los píxeles, el perfil de color y los DPI: el resto de metadatos \
         (EXIF, XMP, IPTC, Content Credentials/C2PA, textos incrustados) no se copia.",
    );
}

fn enhance_controls(
    ui: &mut Ui,
    settings: &mut Settings,
    is_jpeg: bool,
    report: Option<&RestoreReport>,
) {
    ui.add_space(4.0);
    toggle_row(ui, &mut settings.restore_enabled, "Restaurar");
    if settings.restore_enabled {
        let mut preset = settings.preset;
        let options: Vec<(Option<Preset>, &str)> =
            Preset::ALL.iter().map(|&p| (Some(p), p.label())).collect();
        if segmented(ui, &mut preset, &options, true)
            && let Some(preset) = preset
        {
            settings.apply_preset(preset, is_jpeg);
        }
        if settings.preset.is_none() {
            note(ui, "Ajustes manuales");
        }
        let before = settings.restore;
        let r = &mut settings.restore;
        slider_row(
            ui,
            "Bloques JPEG",
            &mut r.deblock,
            "Suaviza la cuadrícula de 8×8 de los JPEG muy comprimidos (solo actúa si la detecta).",
        );
        slider_row(
            ui,
            "Ruido de color",
            &mut r.chroma_noise,
            "Manchas de colores en zonas oscuras.",
        );
        slider_row(
            ui,
            "Grano",
            &mut r.luma_noise,
            "Ruido de luminancia (Non-Local Means). Se adapta al ruido estimado de la foto.",
        );
        slider_row(
            ui,
            "Enfoque",
            &mut r.sharpen,
            "Máscara de enfoque con control de halos.",
        );
        if settings.restore != before {
            settings.preset = None;
        }
        if let Some(report) = report {
            let mut text = format!(
                "Ruido estimado: σ ≈ {} / 255",
                decimal(f64::from(report.noise_sigma_8bit), 1)
            );
            if report.blocking_detected {
                text.push_str(" · bloques JPEG detectados");
            }
            note(ui, text);
        }
    }

    hairline(ui);
    toggle_row(ui, &mut settings.upscale_enabled, "Ampliar");
    if settings.upscale_enabled {
        let u = &mut settings.upscale;
        ui.horizontal(|ui| {
            ui.label("Factor");
            segmented(
                ui,
                &mut u.factor,
                &[(1.5, "×1,5"), (2.0, "×2"), (3.0, "×3"), (4.0, "×4")],
                true,
            );
        });
        form_row(ui, "Método", |ui, label| {
            popup("upscale-method", u.method.label())
                .show_ui(ui, |ui| {
                    for m in UpscaleMethod::ALL {
                        ui.selectable_value(&mut u.method, m, m.label());
                    }
                })
                .response
                .labelled_by(label);
        });
        slider_row(
            ui,
            "Enfoque (RCAS)",
            &mut u.sharpen,
            "Enfoque adaptativo al contraste: recupera nitidez sin halos.",
        );
    }
}

/// Tamaño y memoria de la salida, con aviso si pasa del límite.
fn output_estimate(ui: &mut Ui, settings: &Settings, photo: &Photo) {
    let cfg = settings.pipeline();
    let (w, h) = cfg.output_size(photo.width(), photo.height());
    let mp = f64::from(w) * f64::from(h) / 1e6;
    let ram = cfg.estimate_peak_bytes_for(photo.width(), photo.height(), photo.image.color())
        as f64
        / 1e9;
    let text = format!(
        "Salida: {w} × {h} ({} MP) · RAM ≈ {} GB",
        decimal(mp, 1),
        decimal(ram, 1)
    );
    if mp > settings.max_output_megapixels {
        warning(
            ui,
            format!(
                "{text}\nSupera el límite de {:.0} MP",
                settings.max_output_megapixels
            ),
        );
    } else {
        note(ui, text);
    }
}

fn save_controls(ui: &mut Ui, settings: &mut Settings, photo: Option<&Photo>) {
    form_row(ui, "Formato", |ui, label| {
        popup("format", settings.encode.format.extension().to_uppercase())
            .show_ui(ui, |ui| {
                for format in OutputFormat::ALL {
                    ui.selectable_value(&mut settings.encode.format, format, format.label());
                }
            })
            .response
            .labelled_by(label);
    });
    if settings.encode.format == OutputFormat::Png {
        form_row(ui, "Compresión", |ui, label| {
            popup(
                "png-compression",
                png_short_label(settings.encode.png_compression),
            )
            .show_ui(ui, |ui| {
                for c in [
                    PngCompression::Fast,
                    PngCompression::Balanced,
                    PngCompression::Max,
                ] {
                    ui.selectable_value(&mut settings.encode.png_compression, c, png_label(c));
                }
            })
            .response
            .labelled_by(label);
        });
        if settings.encode.png_compression == PngCompression::Max {
            match photo {
                Some(photo) => {
                    let cfg = settings.pipeline();
                    let (w, h) = cfg.output_size(photo.width(), photo.height());
                    if settings
                        .encode
                        .uses_oxipng(w, h, cfg.output_color(photo.image.color()))
                    {
                        note(ui, "Con oxipng: unos 10 % menos, y tarda varias veces más.");
                    } else {
                        note(
                            ui,
                            "Imagen demasiado grande para oxipng: se usa la compresión máxima \
                             normal.",
                        );
                    }
                }
                None => note(ui, "Con oxipng: unos 10 % menos, y tarda varias veces más."),
            }
        }
    }
    if settings.mode == Mode::Enhance && settings.encode.format != OutputFormat::WebP {
        ui.horizontal(|ui| {
            ui.label("Profundidad");
            segmented(
                ui,
                &mut settings.output_depth,
                &[(BitDepth::Bits16, "16 bits"), (BitDepth::Bits8, "8 bits")],
                true,
            );
        });
    }
    if let Some(photo) = photo
        && settings.mode == Mode::ExactCopy
        && !settings.encode.format.can_store(photo.image.color())
    {
        warning(
            ui,
            format!(
                "{} no puede guardar esta foto ({:?}) sin pérdida: elige {}.",
                settings.encode.format.extension().to_uppercase(),
                photo.image.color(),
                settings.encode.format.alternatives_for(photo.image.color())
            ),
        );
    }
    if let Some(photo) = photo {
        let (w, h) = settings
            .pipeline()
            .output_size(photo.width(), photo.height());
        if let Some(problem) = settings.encode.format.size_problem(w, h) {
            warning(ui, problem);
        }
    }
    if photo.is_some_and(|p| p.info.format == SourceFormat::Jpeg)
        && settings.mode == Mode::ExactCopy
    {
        note(
            ui,
            "De JPEG a PNG el archivo crece (3-10×): es el precio de no perder nada más.",
        );
    }
}

fn export_report(ui: &mut Ui, report: &ExportReport) {
    let p = palette(ui);
    let v = &report.verify;
    let file = report
        .path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if v.identical {
        crate::widgets::dot_label(
            ui,
            p.green,
            format!("Verificado: {} píxeles idénticos", thousands(v.pixels)),
        );
    }
    // Ruta y huella se pueden seleccionar y copiar.
    ui.add(
        egui::Label::new(RichText::new(format!(
            "{file} · {}",
            human_bytes(report.bytes_written)
        )))
        .truncate()
        .selectable(true),
    );
    let details = RichText::new(format!(
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
    .color(p.secondary);
    ui.add(egui::Label::new(details).wrap().selectable(true));
}

/// Número con coma decimal, como se escribe en español.
pub fn decimal(value: f64, places: usize) -> String {
    format!("{value:.places$}").replace('.', ",")
}

pub fn human_bytes(bytes: u64) -> String {
    let b = bytes as f64;
    if b >= 1e9 {
        format!("{} GB", decimal(b / 1e9, 2))
    } else if b >= 1e6 {
        format!("{} MB", decimal(b / 1e6, 1))
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
