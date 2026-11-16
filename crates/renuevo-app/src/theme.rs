//! Aspecto de macOS: la fuente del sistema (San Francisco), los colores de sus temas claro y
//! oscuro y las medidas de sus controles. egui sigue el tema del sistema por su cuenta.

use std::sync::Arc;

use egui::epaint::text::{FontData, FontDefinitions, FontFamily, VariationCoords};
use egui::epaint::{CornerRadius, Shadow, Stroke};
use egui::style::{Selection, TextCursorStyle, WidgetVisuals, Widgets};
use egui::{Color32, FontId, TextStyle, Theme, Ui, Visuals, vec2};

/// Colores con nombre de macOS, para el tema claro o el oscuro.
#[derive(Clone, Copy)]
pub struct Palette {
    /// Fondo de la zona central.
    pub window: Color32,
    /// Barra de herramientas (fundida con la de título).
    pub toolbar: Color32,
    /// Panel de ajustes.
    pub sidebar: Color32,
    /// Fondo detrás de la foto.
    pub canvas: Color32,
    /// Grupos de ajustes, como en Ajustes del Sistema.
    pub card: Color32,
    pub card_stroke: Color32,
    pub separator: Color32,
    pub text: Color32,
    pub secondary: Color32,
    pub tertiary: Color32,
    pub control: Color32,
    pub control_hover: Color32,
    pub control_active: Color32,
    pub control_stroke: Color32,
    /// Fondo del control segmentado y del interruptor apagado.
    pub track: Color32,
    /// Segmento elegido.
    pub segment: Color32,
    /// Raíl de los deslizadores.
    pub rail: Color32,
    /// Bola de deslizadores e interruptores.
    pub knob: Color32,
    pub accent: Color32,
    pub accent_hover: Color32,
    pub green: Color32,
    pub orange: Color32,
    pub red: Color32,
}

pub const DARK: Palette = Palette {
    window: Color32::from_rgb(30, 30, 30),
    toolbar: Color32::from_rgb(42, 42, 42),
    sidebar: Color32::from_rgb(36, 36, 37),
    canvas: Color32::from_rgb(22, 22, 22),
    card: Color32::from_rgb(47, 47, 49),
    card_stroke: Color32::from_rgb(58, 58, 60),
    separator: Color32::from_rgb(56, 56, 58),
    text: Color32::from_rgb(232, 232, 234),
    secondary: Color32::from_rgb(152, 152, 157),
    tertiary: Color32::from_rgb(99, 99, 102),
    control: Color32::from_rgb(76, 76, 78),
    control_hover: Color32::from_rgb(88, 88, 90),
    control_active: Color32::from_rgb(102, 102, 105),
    control_stroke: Color32::from_rgb(84, 84, 86),
    // Más oscuro que la ventana y que los grupos: se ve como un hueco en los dos.
    track: Color32::from_rgb(22, 22, 24),
    segment: Color32::from_rgb(92, 92, 96),
    rail: Color32::from_rgb(84, 84, 88),
    knob: Color32::from_rgb(214, 214, 216),
    accent: Color32::from_rgb(10, 132, 255),
    accent_hover: Color32::from_rgb(64, 156, 255),
    green: Color32::from_rgb(48, 209, 88),
    orange: Color32::from_rgb(255, 159, 10),
    // El rojo «accesible» de macOS en oscuro: el normal no llega a 4,5:1 sobre los grupos.
    red: Color32::from_rgb(255, 105, 97),
};

pub const LIGHT: Palette = Palette {
    window: Color32::from_rgb(250, 250, 250),
    toolbar: Color32::from_rgb(236, 236, 236),
    sidebar: Color32::from_rgb(243, 243, 243),
    canvas: Color32::from_rgb(226, 226, 228),
    card: Color32::from_rgb(255, 255, 255),
    card_stroke: Color32::from_rgb(222, 222, 224),
    separator: Color32::from_rgb(216, 216, 219),
    text: Color32::from_rgb(29, 29, 31),
    secondary: Color32::from_rgb(98, 98, 103),
    tertiary: Color32::from_rgb(168, 168, 172),
    control: Color32::from_rgb(255, 255, 255),
    control_hover: Color32::from_rgb(246, 246, 247),
    control_active: Color32::from_rgb(230, 230, 232),
    control_stroke: Color32::from_rgb(200, 200, 204),
    track: Color32::from_rgb(228, 228, 230),
    segment: Color32::from_rgb(255, 255, 255),
    rail: Color32::from_rgb(196, 196, 200),
    knob: Color32::from_rgb(255, 255, 255),
    accent: Color32::from_rgb(0, 122, 255),
    accent_hover: Color32::from_rgb(0, 106, 230),
    // Las variantes accesibles: el verde y el naranja normales apenas se leen sobre blanco.
    green: Color32::from_rgb(36, 138, 61),
    orange: Color32::from_rgb(178, 96, 0),
    red: Color32::from_rgb(215, 0, 21),
};

pub fn palette(ui: &Ui) -> &'static Palette {
    if ui.visuals().dark_mode {
        &DARK
    } else {
        &LIGHT
    }
}

/// Familia de la seminegrita (títulos y etiquetas destacadas).
pub fn semibold() -> FontFamily {
    FontFamily::Name("semibold".into())
}

/// Instala fuente, colores y medidas para los dos temas.
pub fn install(ctx: &egui::Context) {
    install_fonts(ctx);
    ctx.set_visuals_of(Theme::Dark, visuals(&DARK, true));
    ctx.set_visuals_of(Theme::Light, visuals(&LIGHT, false));
    ctx.all_styles_mut(|style| {
        use FontFamily::{Monospace, Proportional};
        style.text_styles = [
            (TextStyle::Small, FontId::new(11.0, Proportional)),
            (TextStyle::Body, FontId::new(13.0, Proportional)),
            (TextStyle::Button, FontId::new(13.0, Proportional)),
            (TextStyle::Heading, FontId::new(15.0, semibold())),
            (TextStyle::Monospace, FontId::new(12.0, Monospace)),
        ]
        .into();
        let s = &mut style.spacing;
        s.item_spacing = vec2(8.0, 6.0);
        s.button_padding = vec2(10.0, 3.0);
        s.interact_size = vec2(24.0, 24.0);
        s.icon_width = 16.0;
        s.icon_width_inner = 8.0;
        s.icon_spacing = 6.0;
        s.combo_width = 150.0;
        s.slider_rail_height = 4.0;
        s.indent = 16.0;
        s.window_margin = 12.into();
        s.menu_margin = 6.into();
        style.animation_time = 0.12;
        // En macOS el texto de la interfaz no se selecciona (y así arrastrar la barra de
        // herramientas mueve la ventana).
        style.interaction.selectable_labels = false;
    });
}

/// San Francisco, leída del propio sistema: su licencia no permite meterla en la app, pero sí
/// usarla en un Mac. Fuera de macOS (o si cambia de sitio) se quedan las fuentes de egui.
fn install_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    let Ok(bytes) = std::fs::read("/System/Library/Fonts/SFNS.ttf") else {
        ctx.set_fonts(fonts);
        return;
    };
    let blob: egui::epaint::text::Blob = Arc::new(bytes);
    // `opsz` 17 es el tamaño óptico de texto (el de macOS hasta 19 pt): más aire entre letras.
    let variant = |weight: f32| {
        let mut data = FontData::from_blob(Arc::clone(&blob), 0);
        data.tweak.coords = VariationCoords::new([(b"opsz", 17.0), (b"wght", weight)]);
        Arc::new(data)
    };
    fonts.font_data.insert("sf".into(), variant(400.0));
    fonts.font_data.insert("sf-semibold".into(), variant(600.0));

    let fallback = fonts
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    fonts.families.insert(
        FontFamily::Proportional,
        std::iter::once("sf".to_owned())
            .chain(fallback.iter().cloned())
            .collect(),
    );
    fonts.families.insert(
        semibold(),
        std::iter::once("sf-semibold".to_owned())
            .chain(fallback)
            .collect(),
    );
    if let Ok(mono) = std::fs::read("/System/Library/Fonts/SFNSMono.ttf") {
        let mut data = FontData::from_owned(mono);
        data.tweak.coords = VariationCoords::new([(b"wght", 400.0)]);
        fonts.font_data.insert("sf-mono".into(), Arc::new(data));
        if let Some(family) = fonts.families.get_mut(&FontFamily::Monospace) {
            family.insert(0, "sf-mono".to_owned());
        }
    }
    ctx.set_fonts(fonts);
}

fn visuals(p: &Palette, dark: bool) -> Visuals {
    let mut v = if dark {
        Visuals::dark()
    } else {
        Visuals::light()
    };
    let radius = CornerRadius::same(6);
    let widget = |fill: Color32, stroke: Color32| WidgetVisuals {
        bg_fill: fill,
        weak_bg_fill: fill,
        bg_stroke: Stroke::new(if dark { 0.0 } else { 1.0 }, stroke),
        corner_radius: radius,
        fg_stroke: Stroke::new(1.5, p.text),
        expansion: 0.0,
    };
    v.widgets = Widgets {
        noninteractive: WidgetVisuals {
            bg_fill: p.card,
            weak_bg_fill: p.card,
            // Es el color de los separadores.
            bg_stroke: Stroke::new(1.0, p.separator),
            corner_radius: radius,
            fg_stroke: Stroke::new(1.0, p.text),
            expansion: 0.0,
        },
        inactive: widget(p.control, p.control_stroke),
        hovered: widget(p.control_hover, p.control_stroke),
        active: widget(p.control_active, p.control_stroke),
        open: widget(p.control_active, p.control_stroke),
    };
    v.selection = Selection {
        bg_fill: p.accent,
        stroke: Stroke::new(1.0, Color32::WHITE),
    };
    v.weak_text_color = Some(p.secondary);
    v.hyperlink_color = p.accent;
    v.faint_bg_color = if dark {
        Color32::from_rgb(40, 40, 42)
    } else {
        Color32::from_rgb(245, 245, 247)
    };
    v.extreme_bg_color = if dark { p.track } else { Color32::WHITE };
    v.text_edit_bg_color = Some(if dark {
        Color32::from_rgb(28, 28, 30)
    } else {
        Color32::WHITE
    });
    v.warn_fg_color = p.orange;
    v.error_fg_color = p.red;
    v.window_fill = if dark {
        Color32::from_rgb(44, 44, 46)
    } else {
        Color32::WHITE
    };
    v.window_stroke = Stroke::new(1.0, p.card_stroke);
    v.window_corner_radius = CornerRadius::same(10);
    v.menu_corner_radius = CornerRadius::same(8);
    v.window_shadow = Shadow {
        offset: [0, 8],
        blur: 24,
        spread: 0,
        color: Color32::from_black_alpha(if dark { 110 } else { 40 }),
    };
    v.popup_shadow = Shadow {
        offset: [0, 4],
        blur: 14,
        spread: 0,
        color: Color32::from_black_alpha(if dark { 100 } else { 35 }),
    };
    v.panel_fill = p.window;
    v.text_cursor = TextCursorStyle {
        stroke: Stroke::new(2.0, p.accent),
        ..v.text_cursor
    };
    v.slider_trailing_fill = true;
    v.button_frame = true;
    v
}
