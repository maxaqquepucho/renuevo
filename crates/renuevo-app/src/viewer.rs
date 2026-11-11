//! Visor con comparador antes/después, zoom, desplazamiento y vista 1:1.
//!
//! Cada capa sube a la GPU una miniatura (lado ≤ 4096 px) y, cuando se acerca o se aleja mucho
//! el zoom, un recorte de la zona visible a la resolución de la pantalla, calculado en otro hilo. Así no se
//! supera el tamaño máximo de textura aunque la foto ampliada mida 16000 px, y la interfaz no
//! se bloquea mientras se prepara el recorte.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};

use egui::{
    Align2, Color32, ColorImage, FontId, Pos2, Rect, Sense, Stroke, TextureHandle, TextureOptions,
    Ui, Vec2, pos2, vec2,
};

use crate::display::{self, DisplaySource};

pub const PROXY_MAX_SIDE: u32 = 4096;
const FULL_UV: Rect = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
/// Segundos sin interacción antes de generar el recorte detallado.
const IDLE_SECONDS: f64 = 0.15;

pub struct Layer {
    source: Arc<DisplaySource>,
    proxy: TextureHandle,
    /// Los mismos píxeles de la miniatura en la CPU (compartidos con la textura, sin copia):
    /// de aquí sale la versión filtrada de la vista alejada.
    proxy_pixels: Arc<ColorImage>,
    /// Píxeles de la miniatura por píxel de la fuente.
    proxy_scale: f32,
    /// Zona que ocupa la capa, en píxeles de la foto original («antes»).
    region: Rect,
    detail: Option<Detail>,
    /// Recorte que se está preparando en otro hilo.
    pending: Option<Pending>,
    /// Hay un hilo calculando un recorte de esta capa: no se lanza otro hasta que acabe (los
    /// recortes que quedan viejos mientras se mueve el zoom no se acumulan).
    busy: Arc<AtomicBool>,
    name: String,
}

/// Qué recorte es: x, y, ancho, alto en píxeles de la fuente; texels por píxel; filtrado.
#[derive(Debug, Clone, Copy, PartialEq)]
struct DetailKey {
    src: [u32; 4],
    scale: f32,
    nearest: bool,
}

impl DetailKey {
    fn rect(&self) -> Rect {
        Rect::from_min_size(
            pos2(self.src[0] as f32, self.src[1] as f32),
            vec2(self.src[2] as f32, self.src[3] as f32),
        )
    }

    /// ¿Sirve para mostrar `need` con esta escala y filtrado?
    fn serves(&self, need: Rect, scale: f32, nearest: bool) -> bool {
        self.rect().contains_rect(need.shrink(0.5))
            && (self.scale / scale - 1.0).abs() < 0.15
            && self.nearest == nearest
    }

    /// ¿Mejora lo que ya da la miniatura a esta escala? Un recorte de otra escala (p. ej. el de
    /// la vista ajustada justo después de pulsar «1:1», o el de 1:1 al volver a ajustar) se vería
    /// pixelado o con moiré: entonces mejor solo la miniatura hasta que llegue el nuevo.
    fn worth_painting(&self, target: f32, proxy_scale: f32) -> bool {
        let s = self.scale;
        (s >= target * 0.85 && s <= target * 1.5) || (s < target && s > proxy_scale * 1.05)
    }
}

struct Detail {
    key: DetailKey,
    tex: TextureHandle,
}

struct Pending {
    key: DetailKey,
    rx: Receiver<ColorImage>,
}

impl Layer {
    pub fn new(
        ctx: &egui::Context,
        name: &str,
        source: Arc<DisplaySource>,
        proxy: ColorImage,
        region: Rect,
    ) -> Self {
        let proxy_scale = proxy.size[0] as f32 / source.width().max(1) as f32;
        let proxy_pixels = Arc::new(proxy);
        let proxy = ctx.load_texture(
            format!("{name}-proxy"),
            Arc::clone(&proxy_pixels),
            TextureOptions::LINEAR,
        );
        Self {
            source,
            proxy,
            proxy_pixels,
            proxy_scale,
            region,
            detail: None,
            pending: None,
            busy: Arc::new(AtomicBool::new(false)),
            name: name.to_owned(),
        }
    }

    pub fn source(&self) -> &Arc<DisplaySource> {
        &self.source
    }

    /// Zona que cubre la capa, en píxeles de la foto original.
    pub fn region(&self) -> Rect {
        self.region
    }

    pub fn covers_whole_image(&self, image_size: Vec2) -> bool {
        self.region.min == Pos2::ZERO && (self.region.max.to_vec2() - image_size).length() < 0.5
    }

    /// Dibuja la capa dentro del recorte del `painter`.
    fn paint(
        &mut self,
        ctx: &egui::Context,
        painter: &egui::Painter,
        img_rect: Rect,
        zoom: f32,
        ppp: f32,
        idle: bool,
    ) {
        let to_screen = |p: Pos2| img_rect.min + p.to_vec2() * zoom;
        let layer_rect = Rect::from_min_max(to_screen(self.region.min), to_screen(self.region.max));
        painter.image(self.proxy.id(), layer_rect, FULL_UV, Color32::WHITE);

        let (sw, sh) = (self.source.width() as f32, self.source.height() as f32);
        // Píxeles físicos de pantalla por píxel de la fuente.
        let pps = layer_rect.width() * ppp / sw.max(1.0);
        // Muy reducida, la miniatura (wgpu no hace mipmaps) parpadea y crea moiré en los
        // detalles finos: también entonces se prepara una versión filtrada (Lanczos) a la
        // resolución de la pantalla, igual que el recorte nítido al acercar.
        let minified = pps < self.proxy_scale * 0.7;
        if !minified && pps <= self.proxy_scale * 1.15 {
            // La miniatura basta.
            self.detail = None;
            self.pending = None;
            return;
        }

        // ¿Llegó el recorte que se estaba preparando?
        if let Some(pending) = &self.pending
            && let Ok(image) = pending.rx.try_recv()
        {
            let options = if pending.key.nearest {
                TextureOptions::NEAREST
            } else {
                TextureOptions::LINEAR
            };
            let tex = ctx.load_texture(format!("{}-detail", self.name), image, options);
            self.detail = Some(Detail {
                key: pending.key,
                tex,
            });
            self.pending = None;
        }

        // Nunca más grande que lo que admite la GPU (8192 px en eframe/wgpu).
        let max_side = ctx.input(|i| i.max_texture_side) as f32;
        let visible = layer_rect.intersect(painter.clip_rect());
        let mut target = pps.min(1.0);
        if visible.width() > 0.0 && visible.height() > 0.0 {
            let to_src = |p: Pos2| {
                pos2(
                    (p.x - layer_rect.min.x) / layer_rect.width() * sw,
                    (p.y - layer_rect.min.y) / layer_rect.height() * sh,
                )
            };
            let need = Rect::from_min_max(to_src(visible.min), to_src(visible.max));
            let bounds = Rect::from_min_size(Pos2::ZERO, vec2(sw, sh));
            let want = need.expand2(need.size() * 0.2).intersect(bounds);
            target = target
                .min(max_side / want.width().max(1.0))
                .min(max_side / want.height().max(1.0));
            let nearest = pps >= 2.0;
            let covered = self
                .detail
                .as_ref()
                .is_some_and(|d| d.key.serves(need, target, nearest))
                || self
                    .pending
                    .as_ref()
                    .is_some_and(|p| p.key.serves(need, target, nearest));
            if !covered && idle && !self.busy.load(Ordering::Acquire) {
                let x = want.min.x.floor().max(0.0) as u32;
                let y = want.min.y.floor().max(0.0) as u32;
                let w = (want.max.x.ceil() as u32)
                    .min(sw as u32)
                    .saturating_sub(x)
                    .max(1);
                let h = (want.max.y.ceil() as u32)
                    .min(sh as u32)
                    .saturating_sub(y)
                    .max(1);
                let max_px = max_side as u32;
                let out_w = ((w as f32 * target).round() as u32).clamp(1, max_px);
                let out_h = ((h as f32 * target).round() as u32).clamp(1, max_px);
                let (tx, rx) = mpsc::channel();
                let source = Arc::clone(&self.source);
                let proxy = Arc::clone(&self.proxy_pixels);
                let proxy_scale = f64::from(self.proxy_scale);
                // Con la vista alejada, la miniatura tiene resolución de sobra: se reduce ella
                // (rápido) en lugar de recorrer la foto entera.
                let from_proxy = target < self.proxy_scale;
                let busy = Arc::clone(&self.busy);
                busy.store(true, Ordering::Release);
                let repaint = ctx.clone();
                std::thread::spawn(move || {
                    let image = if from_proxy {
                        let zone = [x, y, w, h].map(|v| f64::from(v) * proxy_scale);
                        display::downscale(&proxy, zone, out_w, out_h)
                    } else {
                        source.crop(x, y, w, h, out_w, out_h)
                    };
                    // Si mientras tanto se pidió otro recorte, nadie escucha: se descarta.
                    let _ = tx.send(image);
                    busy.store(false, Ordering::Release);
                    repaint.request_repaint();
                });
                self.pending = Some(Pending {
                    key: DetailKey {
                        src: [x, y, w, h],
                        scale: target,
                        nearest,
                    },
                    rx,
                });
            }
        }

        if let Some(Detail { key: d, tex }) = &self.detail
            && d.worth_painting(target, self.proxy_scale)
        {
            let src_to_screen = |sx: f32, sy: f32| {
                pos2(
                    layer_rect.min.x + sx / sw * layer_rect.width(),
                    layer_rect.min.y + sy / sh * layer_rect.height(),
                )
            };
            let rect = snap_rect(
                src_to_screen(d.src[0] as f32, d.src[1] as f32),
                vec2(
                    d.src[2] as f32 / sw * layer_rect.width(),
                    d.src[3] as f32 / sh * layer_rect.height(),
                ),
                ppp,
            );
            painter.image(tex.id(), rect, FULL_UV, Color32::WHITE);
        }
    }
}

/// Lleva un punto al píxel físico más cercano (las mitades, siempre hacia el mismo lado).
fn snap_point(p: Pos2, ppp: f32) -> Pos2 {
    let snap = |v: f32| (v * ppp + 0.5).floor() / ppp;
    pos2(snap(p.x), snap(p.y))
}

/// Rectángulo ajustado a píxeles físicos sin cambiar su tamaño en píxeles: ajustar las dos
/// esquinas por separado puede estirarlo un píxel, y entonces el filtrado lineal vuelve a
/// muestrear toda la textura y la vista 1:1 se ve borrosa.
pub(crate) fn snap_rect(min: Pos2, size: Vec2, ppp: f32) -> Rect {
    let size = vec2((size.x * ppp).round() / ppp, (size.y * ppp).round() / ppp);
    Rect::from_min_size(snap_point(min, ppp), size)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompareMode {
    Split,
    Before,
    After,
}

pub struct Viewer {
    /// Puntos de pantalla por píxel de la foto original.
    zoom: f32,
    /// Píxel de la foto que queda en el centro de la vista.
    center: Pos2,
    /// Posición del divisor (0..1 del ancho de la vista).
    split: f32,
    pub compare: CompareMode,
    needs_fit: bool,
    dragging_split: bool,
    last_interaction: f64,
    last_view: Rect,
    image_size: Vec2,
}

impl Default for Viewer {
    fn default() -> Self {
        Self {
            zoom: 1.0,
            center: Pos2::ZERO,
            split: 0.5,
            compare: CompareMode::Split,
            needs_fit: true,
            dragging_split: false,
            last_interaction: f64::NEG_INFINITY,
            last_view: Rect::NOTHING,
            image_size: Vec2::ZERO,
        }
    }
}

impl Viewer {
    pub fn reset(&mut self, image_size: Vec2) {
        self.image_size = image_size;
        self.needs_fit = true;
        self.split = 0.5;
    }

    pub fn request_fit(&mut self) {
        self.needs_fit = true;
    }

    pub fn one_to_one(&mut self, ppp: f32) {
        self.zoom = 1.0 / ppp;
    }

    /// Zoom en % de píxeles físicos (100 % = un píxel de la foto por píxel de pantalla).
    pub fn zoom_percent(&self, ppp: f32) -> f32 {
        self.zoom * ppp * 100.0
    }

    /// Zona visible en píxeles de la foto original.
    pub fn visible_region(&self) -> Option<Rect> {
        if !self.last_view.is_positive() || self.zoom <= 0.0 {
            return None;
        }
        let half = self.last_view.size() / (2.0 * self.zoom);
        let r = Rect::from_min_max(self.center - half, self.center + half)
            .intersect(Rect::from_min_size(Pos2::ZERO, self.image_size));
        (r.width() >= 1.0 && r.height() >= 1.0).then_some(r)
    }

    pub fn is_idle(&self, now: f64) -> bool {
        now - self.last_interaction > IDLE_SECONDS
    }

    fn fit(&mut self, view: Rect) {
        if self.image_size.x <= 0.0 || self.image_size.y <= 0.0 {
            return;
        }
        let z = (view.width() / self.image_size.x).min(view.height() / self.image_size.y) * 0.98;
        self.zoom = z.max(1e-4);
        self.center = (self.image_size / 2.0).to_pos2();
    }

    pub fn ui(
        &mut self,
        ui: &mut Ui,
        before: Option<&mut Layer>,
        after: Option<&mut Layer>,
        hint: Option<&str>,
    ) {
        let (view, response) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
        self.last_view = view;
        let painter = ui.painter_at(view);
        painter.rect_filled(view, 0.0, Color32::from_gray(22));

        let Some(before) = before else {
            painter.text(
                view.center(),
                Align2::CENTER_CENTER,
                "Arrastra una foto aquí o pulsa «Abrir foto…»",
                FontId::proportional(18.0),
                Color32::GRAY,
            );
            return;
        };
        if self.needs_fit {
            self.fit(view);
            self.needs_fit = false;
        }
        let ctx = ui.ctx().clone();
        let now = ui.input(|i| i.time);
        let ppp = ctx.pixels_per_point();
        let has_after = after.is_some();

        // Pellizco o ⌘+rueda: zoom alrededor del puntero. Rueda/dos dedos: desplazar.
        if let Some(hover) = response.hover_pos() {
            let (scroll, zoom_delta) = ui.input(|i| (i.smooth_scroll_delta, i.zoom_delta()));
            if (zoom_delta - 1.0).abs() > 1e-4 {
                let anchor = self.center + (hover - view.center()) / self.zoom;
                self.zoom = (self.zoom * zoom_delta).clamp(0.005, 64.0);
                self.center = anchor - (hover - view.center()) / self.zoom;
                self.last_interaction = now;
            }
            if scroll != Vec2::ZERO {
                self.center -= scroll / self.zoom;
                self.last_interaction = now;
            }
        }

        let split_x = view.left() + self.split * view.width();
        if response.drag_started() {
            // Se decide por dónde se pulsó, no por dónde está el puntero al detectar el arrastre.
            let press = ui.input(|i| i.pointer.press_origin());
            self.dragging_split = self.compare == CompareMode::Split
                && has_after
                && press
                    .or(response.interact_pointer_pos())
                    .is_some_and(|p| (p.x - split_x).abs() < 12.0);
        }
        if response.dragged() {
            if self.dragging_split {
                if let Some(p) = response.interact_pointer_pos() {
                    self.split = ((p.x - view.left()) / view.width()).clamp(0.0, 1.0);
                }
                self.last_interaction = now;
            } else {
                self.center -= response.drag_delta() / self.zoom;
                self.last_interaction = now;
            }
        }
        if response.drag_stopped() {
            self.dragging_split = false;
        }
        if response.double_clicked() {
            self.fit(view);
        }
        if let Some(p) = response.hover_pos()
            && has_after
            && self.compare == CompareMode::Split
            && (p.x - split_x).abs() < 12.0
        {
            ctx.set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
        }

        // El origen, en un píxel físico: así, a 1:1, cada píxel de la foto cae en uno de la
        // pantalla y el filtrado lineal no lo mezcla con el vecino.
        let img_rect = Rect::from_min_size(
            snap_point(view.center() - self.center.to_vec2() * self.zoom, ppp),
            self.image_size * self.zoom,
        );
        let idle = self.is_idle(now);
        let split_x = view.left() + self.split * view.width();

        before.paint(&ctx, &painter, img_rect, self.zoom, ppp, idle);

        if let Some(after) = after {
            let after_clip = match self.compare {
                CompareMode::Split => Some(Rect::from_min_max(pos2(split_x, view.min.y), view.max)),
                CompareMode::After => Some(view),
                CompareMode::Before => None,
            };
            if let Some(clip) = after_clip {
                let after_painter = painter.with_clip_rect(clip);
                if !after.covers_whole_image(self.image_size) {
                    // Vista previa de una zona: se oscurece lo que no está procesado.
                    after_painter.rect_filled(clip, 0.0, Color32::from_black_alpha(150));
                }
                after.paint(&ctx, &after_painter, img_rect, self.zoom, ppp, idle);
            }
            if self.compare == CompareMode::Split {
                painter.line_segment(
                    [pos2(split_x, view.min.y), pos2(split_x, view.max.y)],
                    Stroke::new(2.0, Color32::WHITE),
                );
                painter.circle_filled(pos2(split_x, view.center().y), 7.0, Color32::WHITE);
                label(
                    &painter,
                    pos2(split_x - 8.0, view.min.y + 8.0),
                    Align2::RIGHT_TOP,
                    "ANTES",
                );
                label(
                    &painter,
                    pos2(split_x + 8.0, view.min.y + 8.0),
                    Align2::LEFT_TOP,
                    "DESPUÉS",
                );
            }
        }

        label(
            &painter,
            view.left_bottom() + vec2(8.0, -8.0),
            Align2::LEFT_BOTTOM,
            &format!("{:.0} %", self.zoom_percent(ppp)),
        );
        if let Some(hint) = hint {
            label(
                &painter,
                view.center_bottom() + vec2(0.0, -8.0),
                Align2::CENTER_BOTTOM,
                hint,
            );
        }
        if !idle {
            // Tras dejar de mover, volver a pintar para generar el recorte nítido.
            ctx.request_repaint_after(std::time::Duration::from_secs_f64(IDLE_SECONDS + 0.02));
        }
    }
}

fn label(painter: &egui::Painter, pos: Pos2, align: Align2, text: &str) {
    let galley =
        painter.layout_no_wrap(text.to_owned(), FontId::proportional(13.0), Color32::WHITE);
    let rect = align.anchor_size(pos, galley.size()).expand(4.0);
    painter.rect_filled(rect, 4.0, Color32::from_black_alpha(160));
    painter.galley(rect.min + vec2(4.0, 4.0), galley, Color32::WHITE);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapped_rect_keeps_its_size_in_physical_pixels() {
        // Con las esquinas en medio píxel, redondearlas por separado lo estiraba a 1024.
        for ppp in [1.0, 2.0] {
            let r = snap_rect(pos2(-65.5 / ppp, -65.25), vec2(1023.0, 1023.0) / ppp, ppp);
            assert_eq!(r.size() * ppp, vec2(1023.0, 1023.0));
            assert_eq!((r.min.x * ppp).fract(), 0.0);
            assert_eq!((r.min.y * ppp).fract(), 0.0);
        }
    }
}
