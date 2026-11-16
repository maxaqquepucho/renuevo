//! Controles con aspecto de macOS hechos con egui: grupos, control segmentado, interruptor,
//! deslizador, filas de formulario y botón principal.

use std::ops::RangeInclusive;

use egui::{
    Align, Color32, CornerRadius, FontId, Frame, Layout, Margin, Response, RichText, Role, Sense,
    Stroke, Ui, WidgetInfo, pos2, vec2,
};

use crate::theme::{palette, semibold};

/// Título de un grupo de ajustes, encima de él (como en Ajustes del Sistema).
pub fn section(ui: &mut Ui, title: &str) {
    ui.add_space(10.0);
    ui.label(
        RichText::new(title)
            .font(FontId::new(13.0, semibold()))
            .color(palette(ui).text),
    );
    ui.add_space(2.0);
}

/// Grupo de ajustes: fondo propio, esquinas redondeadas y todo el ancho.
pub fn card<R>(ui: &mut Ui, add_contents: impl FnOnce(&mut Ui) -> R) -> R {
    let p = palette(ui);
    Frame::new()
        .fill(p.card)
        .stroke(Stroke::new(1.0, p.card_stroke))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::symmetric(12, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add_contents(ui)
        })
        .inner
}

/// Separador fino dentro de un grupo.
pub fn hairline(ui: &mut Ui) {
    let p = palette(ui);
    ui.add_space(2.0);
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
    ui.painter().hline(
        rect.x_range(),
        rect.center().y,
        Stroke::new(1.0, p.separator),
    );
    ui.add_space(2.0);
}

/// Texto pequeño y secundario que ocupa todo el ancho.
pub fn note(ui: &mut Ui, text: impl Into<String>) {
    let color = palette(ui).secondary;
    ui.add(egui::Label::new(RichText::new(text).small().color(color)).wrap());
}

/// Aviso en naranja.
pub fn warning(ui: &mut Ui, text: impl Into<String>) {
    let color = palette(ui).orange;
    ui.add(egui::Label::new(RichText::new(text).color(color)).wrap());
}

/// Fila de formulario: la etiqueta a la izquierda y el control alineado a la derecha. El
/// control recibe el `Id` de la etiqueta para nombrarse con ella (`Response::labelled_by`).
pub fn form_row<R>(ui: &mut Ui, label: &str, add_right: impl FnOnce(&mut Ui, egui::Id) -> R) -> R {
    ui.horizontal(|ui| {
        let label_id = ui.label(label).id;
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            add_right(ui, label_id)
        })
        .inner
    })
    .inner
}

/// Control segmentado de macOS. Con `fill`, los segmentos se reparten todo el ancho.
/// Devuelve `true` si cambia la elección.
pub fn segmented<T: PartialEq + Copy>(
    ui: &mut Ui,
    current: &mut T,
    options: &[(T, &str)],
    fill: bool,
) -> bool {
    let p = palette(ui);
    let mut changed = false;
    Frame::new()
        .fill(p.track)
        .corner_radius(CornerRadius::same(7))
        .inner_margin(Margin::same(2))
        .show(ui, |ui| {
            let v = ui.visuals_mut();
            v.selection.bg_fill = p.segment;
            v.selection.stroke = Stroke::new(1.0, p.text);
            for w in [&mut v.widgets.hovered, &mut v.widgets.active] {
                w.weak_bg_fill = p.track.lerp_to_gamma(p.segment, 0.35);
                w.bg_stroke = Stroke::NONE;
            }
            for w in [
                &mut v.widgets.inactive,
                &mut v.widgets.hovered,
                &mut v.widgets.active,
            ] {
                w.corner_radius = CornerRadius::same(5);
            }
            ui.spacing_mut().item_spacing.x = 2.0;
            ui.spacing_mut().button_padding = vec2(10.0, 1.0);
            // Los botones miden como mínimo `interact_size.y`: se baja para que el control
            // entero (con su margen de 2 pt) mida lo mismo que un botón normal.
            let height = ui.spacing().interact_size.y - 4.0;
            ui.spacing_mut().interact_size.y = height;
            let width = if fill {
                let n = options.len().max(1) as f32;
                ((ui.available_width() - 2.0 * (n - 1.0)) / n).max(0.0)
            } else {
                0.0
            };
            ui.horizontal(|ui| {
                for &(value, label) in options {
                    let selected = *current == value;
                    let button =
                        egui::Button::selectable(selected, label).min_size(vec2(width, height));
                    if ui.add(button).clicked() && !selected {
                        *current = value;
                        changed = true;
                    }
                }
            });
        });
    changed
}

/// Fila con un texto y un interruptor a la derecha; se pulsa en cualquier punto de la fila.
pub fn toggle_row(ui: &mut Ui, on: &mut bool, label: &str) -> Response {
    let p = palette(ui);
    let height = ui.spacing().interact_size.y;
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());
    // Id fijo (por el nombre), no por posición: si aparece una nota encima, el interruptor no
    // pierde el foco del teclado.
    let mut response = ui.interact(
        rect,
        ui.make_persistent_id(("switch", label)),
        Sense::click(),
    );
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    let enabled = ui.is_enabled();
    let on_now = *on;
    response.widget_info(|| WidgetInfo::selected(Role::Switch, enabled, on_now, label));

    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        painter.text(
            pos2(rect.left(), rect.center().y),
            egui::Align2::LEFT_CENTER,
            label,
            FontId::new(13.0, semibold()),
            p.text,
        );
        let t = ui.ctx().animate_bool_responsive(response.id, *on);
        let size = vec2(32.0, 18.0);
        let track = egui::Rect::from_min_size(
            pos2(rect.right() - size.x, rect.center().y - size.y / 2.0),
            size,
        );
        let fill = p.rail.lerp_to_gamma(p.accent, t);
        painter.rect_filled(track, CornerRadius::same(9), fill);
        if response.has_focus() {
            own_focus_ring(ui, response.id);
            painter.rect_stroke(
                track.expand(2.5),
                CornerRadius::same(12),
                focus_stroke(p),
                egui::StrokeKind::Middle,
            );
        }
        let r = size.y / 2.0 - 1.5;
        let x = egui::lerp(track.left() + r + 1.5..=track.right() - r - 1.5, t);
        let center = pos2(x, track.center().y);
        painter.circle_filled(
            center + vec2(0.0, 0.6),
            r + 0.4,
            Color32::from_black_alpha(45),
        );
        painter.circle_filled(center, r, Color32::WHITE);
    }
    response
}

/// Deslizador de macOS: raíl fino, tramo recorrido en el color de acento y bola blanca.
/// Con el foco, las flechas lo mueven un 1 %.
pub fn slider(ui: &mut Ui, value: &mut f32, range: RangeInclusive<f32>, label: &str) -> Response {
    let p = palette(ui);
    let (lo, hi) = (*range.start(), *range.end());
    let knob = 8.0;
    let (rect, _) =
        ui.allocate_exact_size(vec2(ui.available_width(), 2.0 * knob + 4.0), Sense::hover());
    // Id fijo (por el nombre): al pasar a «Ajustes manuales» aparece una nota encima de los
    // deslizadores, y con un id por posición el que se está moviendo perdería el foco.
    let mut response = ui.interact(
        rect,
        ui.make_persistent_id(("slider", label)),
        Sense::click_and_drag(),
    );
    let rail = rect.shrink2(vec2(knob, 0.0));
    let before = *value;
    if let Some(pos) = response.interact_pointer_pos()
        && (response.dragged() || response.clicked() || response.is_pointer_button_down_on())
    {
        let t = ((pos.x - rail.left()) / rail.width()).clamp(0.0, 1.0);
        *value = lo + t * (hi - lo);
    }
    // Teclado y lectores de pantalla, como `egui::Slider`: con el foco, las flechas
    // horizontales mueven el valor (y no el foco); VoiceOver puede subir, bajar o fijarlo.
    let step = (hi - lo) / 100.0;
    let id = response.id;
    // Se lee antes de `ui.input`: consultar el foco (la memoria de egui) dentro de ese cierre
    // vuelve a tomar el cerrojo del contexto, y si otro hilo espera para escribir (un trabajo
    // que pide repintar), se bloquean los dos.
    let focused = response.has_focus();
    if focused {
        ui.memory_mut(|m| {
            m.set_focus_lock_filter(
                id,
                egui::EventFilter {
                    horizontal_arrows: true,
                    ..Default::default()
                },
            );
        });
        own_focus_ring(ui, id);
    }
    let steps = ui.input(|i| {
        use egui::accesskit::Action;
        let keys = if focused {
            i.num_presses(egui::Key::ArrowRight) as f32 - i.num_presses(egui::Key::ArrowLeft) as f32
        } else {
            0.0
        };
        keys + i.num_accesskit_action_requests(id, Action::Increment) as f32
            - i.num_accesskit_action_requests(id, Action::Decrement) as f32
    });
    *value = (*value + steps * step).clamp(lo, hi);
    let requested = ui.input(|i| {
        use egui::accesskit::{Action, ActionData};
        i.accesskit_action_requests(id, Action::SetValue)
            .filter_map(|r| match r.data {
                Some(ActionData::NumericValue(v)) => Some(v),
                _ => None,
            })
            .last()
    });
    if let Some(v) = requested {
        *value = (v as f32).clamp(lo, hi);
    }
    if *value != before {
        response.mark_changed();
    }
    let enabled = ui.is_enabled();
    let shown = f64::from(*value);
    response.widget_info(|| WidgetInfo::slider(enabled, shown, label));
    ui.ctx().accesskit_node_builder(id, |node| {
        use egui::accesskit::Action;
        node.set_min_numeric_value(f64::from(lo));
        node.set_max_numeric_value(f64::from(hi));
        node.set_numeric_value_step(f64::from(step));
        node.add_action(Action::SetValue);
        if *value < hi {
            node.add_action(Action::Increment);
        }
        if *value > lo {
            node.add_action(Action::Decrement);
        }
    });

    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let t = if hi > lo {
            (*value - lo) / (hi - lo)
        } else {
            0.0
        };
        let y = rect.center().y;
        let x = egui::lerp(rail.left()..=rail.right(), t);
        let bar = |x0: f32, x1: f32, color| {
            painter.rect_filled(
                egui::Rect::from_min_max(pos2(x0, y - 2.0), pos2(x1, y + 2.0)),
                CornerRadius::same(2),
                color,
            );
        };
        bar(rail.left(), rail.right(), p.rail);
        bar(rail.left(), x, p.accent);
        let center = pos2(x, y);
        painter.circle_filled(
            center + vec2(0.0, 0.8),
            knob + 0.5,
            Color32::from_black_alpha(50),
        );
        painter.circle(center, knob, p.knob, Stroke::new(0.5, p.control_stroke));
        if response.has_focus() {
            painter.circle_stroke(center, knob + 2.5, focus_stroke(p));
        }
    }
    response
}

/// Ancho de la columna de nombres de los deslizadores (cabe «Enfoque (RCAS)»).
const SLIDER_LABEL_WIDTH: f32 = 104.0;
/// Ancho de la columna del valor («100 %»).
const SLIDER_VALUE_WIDTH: f32 = 36.0;

/// Deslizador de 0 a 1 en una fila: nombre, deslizador y valor en %.
pub fn slider_row(ui: &mut Ui, label: &str, value: &mut f32, help: &str) -> Response {
    let p = palette(ui);
    ui.horizontal(|ui| {
        let height = ui.spacing().interact_size.y;
        ui.allocate_ui_with_layout(
            vec2(SLIDER_LABEL_WIDTH, height),
            Layout::left_to_right(Align::Center),
            |ui| {
                ui.set_min_width(SLIDER_LABEL_WIDTH);
                ui.add(egui::Label::new(label).truncate())
                    .on_hover_text(help);
            },
        );
        let slider_width = (ui.available_width() - SLIDER_VALUE_WIDTH - 8.0).max(40.0);
        let response = ui
            .allocate_ui_with_layout(
                vec2(slider_width, height),
                Layout::left_to_right(Align::Center),
                |ui| slider(ui, value, 0.0..=1.0, label),
            )
            .inner
            .on_hover_text(help);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(RichText::new(format!("{:.0} %", *value * 100.0)).color(p.secondary));
        });
        response
    })
    .inner
}

/// Botón principal: relleno del color de acento y texto blanco. Con `full_width`, ocupa la fila.
pub fn primary_button(ui: &mut Ui, text: &str, enabled: bool, full_width: bool) -> Response {
    let p = palette(ui);
    let width = if full_width {
        ui.available_width()
    } else {
        0.0
    };
    if !enabled {
        // Desactivado es un botón normal apagado, como en macOS (no azul a medias).
        return ui.add_enabled(false, egui::Button::new(text).min_size(vec2(width, 28.0)));
    }
    ui.scope(|ui| {
        let v = ui.visuals_mut();
        for (w, fill) in [
            (&mut v.widgets.inactive, p.accent),
            (&mut v.widgets.hovered, p.accent_hover),
            (&mut v.widgets.active, p.accent_hover),
        ] {
            w.weak_bg_fill = fill;
            w.bg_fill = fill;
            w.bg_stroke = Stroke::NONE;
            w.fg_stroke = Stroke::new(1.0, Color32::WHITE);
        }
        let button = egui::Button::new(RichText::new(text).color(Color32::WHITE))
            .min_size(vec2(width, 28.0));
        ui.add(button)
    })
    .inner
}

/// Menú desplegable con las dos flechas (arriba y abajo) de los de macOS.
pub fn popup(id_salt: &str, selected: impl Into<egui::WidgetText>) -> egui::ComboBox<'_> {
    egui::ComboBox::from_id_salt(id_salt)
        .selected_text(selected)
        .icon(|ui, rect, visuals, _open| {
            let c = rect.center();
            let stroke = Stroke::new(1.5, visuals.fg_stroke.color);
            for dir in [-1.0f32, 1.0] {
                ui.painter().add(egui::Shape::line(
                    vec![
                        c + vec2(-3.5, dir * 1.5),
                        c + vec2(0.0, dir * 5.0),
                        c + vec2(3.5, dir * 1.5),
                    ],
                    stroke,
                ));
            }
        })
}

/// Punto de color delante de un texto (estado de un archivo, mensajes).
/// El texto se puede seleccionar y copiar (rutas, errores).
pub fn dot_label(ui: &mut Ui, color: Color32, text: impl Into<egui::WidgetText>) -> Response {
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(vec2(8.0, 8.0), Sense::hover());
        ui.painter().circle_filled(rect.center(), 4.0, color);
        ui.add(egui::Label::new(text).selectable(true))
    })
    .inner
}

/// El anillo de foco de macOS: el color de acento, semitransparente.
fn focus_stroke(p: &crate::theme::Palette) -> Stroke {
    Stroke::new(3.0, p.accent.gamma_multiply(0.55))
}

fn own_ring_key() -> egui::Id {
    egui::Id::unique("renuevo-own-focus-ring")
}

/// El widget `id` dibuja su propio anillo de foco (deslizador e interruptor).
fn own_focus_ring(ui: &Ui, id: egui::Id) {
    ui.ctx().data_mut(|d| d.insert_temp(own_ring_key(), id));
}

/// Anillo de foco alrededor del control que tiene el foco del teclado (o del campo de texto
/// en el que se escribe). egui no da el foco a los botones al pulsarlos con el ratón, así que
/// solo aparece al moverse con el tabulador, como en macOS. Se llama al final del fotograma.
pub fn paint_focus_ring(ctx: &egui::Context) {
    let Some(id) = ctx.memory(|m| m.focused()) else {
        return;
    };
    if ctx.data(|d| d.get_temp::<egui::Id>(own_ring_key())) == Some(id) {
        return;
    }
    let Some(response) = ctx.read_response(id) else {
        return;
    };
    if !response.interact_rect.is_positive() {
        return;
    }
    let p = if ctx.theme() == egui::Theme::Dark {
        &crate::theme::DARK
    } else {
        &crate::theme::LIGHT
    };
    ctx.layer_painter(response.layer_id)
        .with_clip_rect(response.interact_rect.expand(4.0))
        .rect_stroke(
            response.rect.expand(1.5),
            CornerRadius::same(8),
            focus_stroke(p),
            egui::StrokeKind::Outside,
        );
}
