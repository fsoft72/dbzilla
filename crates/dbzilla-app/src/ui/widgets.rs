//! The Obsidian widget kit. Screens compose these; they never style widgets inline.

use super::{ToastKind, Toasts};
use crate::glyphs::{self, Ico};
use crate::theme;
use eframe::egui::{
    self, Align, Align2, Area, Color32, CursorIcon, FontId, Frame, InnerResponse, Layout, Margin, Order, Response, RichText, Sense, Shadow, Stroke, StrokeKind, TextEdit, Ui, Vec2,
};
use std::time::Duration;

/// Size of icon buttons.
pub const ICON_BUTTON: f32 = 28.0;
/// Size of rail tiles.
pub const RAIL_TILE: f32 = 44.0;
/// Text input inner margin.
const INPUT_MARGIN: Margin = Margin { left: 8, right: 8, top: 9, bottom: 9 };
/// Text size of buttons.
const BUTTON_TEXT: f32 = 13.5;

/// Heading text (22 px semi-bold).
pub fn heading(ui: &mut Ui, text: &str) -> Response {
    ui.label(RichText::new(text).font(FontId::new(22.0, theme::semibold())).color(theme::TEXT))
}

/// Semi-bold rich text.
pub fn semibold(text: impl Into<String>, size: f32) -> RichText {
    RichText::new(text.into()).font(FontId::new(size, theme::semibold()))
}

/// Small muted text.
pub fn muted(ui: &mut Ui, text: &str) -> Response {
    ui.label(RichText::new(text).size(12.0).color(theme::MUTED))
}

/// Small uppercase label above a value or input.
pub fn field_label(ui: &mut Ui, text: &str) -> Response {
    ui.label(RichText::new(text.to_uppercase()).font(FontId::new(11.0, theme::semibold())).color(theme::LABEL))
}

/// Section title inside a form: uppercase accent text followed by a separator.
pub fn section_title(ui: &mut Ui, text: &str) {
    ui.add_space(10.0);
    ui.label(RichText::new(text.to_uppercase()).font(FontId::new(12.0, theme::semibold())).color(theme::SECONDARY));
    ui.separator();
}

/// Colours and look of a custom painted button.
struct ButtonLook {
    idle: Color32,
    hover: Color32,
    active: Color32,
    stroke: Stroke,
    text: Color32,
}

/// Paints a button of the kit with the given look.
fn kit_button(ui: &mut Ui, text: &str, look: ButtonLook, min: Option<Vec2>) -> Response {
    let font = FontId::new(BUTTON_TEXT, theme::semibold());
    let galley = ui.painter().layout_no_wrap(text.to_owned(), font, look.text);
    let pad = egui::vec2(14.0, 6.0);
    let mut size = galley.size() + pad * 2.0;
    size.y = size.y.max(ui.spacing().interact_size.y);
    if let Some(m) = min {
        size = size.max(m);
    }
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    if ui.is_rect_visible(rect) {
        let fill = if !ui.is_enabled() {
            look.idle.gamma_multiply(0.5)
        } else if resp.is_pointer_button_down_on() {
            look.active
        } else if resp.hovered() {
            look.hover
        } else {
            look.idle
        };
        ui.painter().rect_filled(rect, theme::RADIUS_CTRL, fill);
        if look.stroke != Stroke::NONE {
            ui.painter().rect_stroke(rect, theme::RADIUS_CTRL, look.stroke, StrokeKind::Inside);
        }
        let color = if ui.is_enabled() { look.text } else { look.text.gamma_multiply(0.5) };
        ui.painter().galley(rect.center() - galley.size() / 2.0, galley, color);
    }
    if ui.is_enabled() { resp.on_hover_cursor(CursorIcon::PointingHand) } else { resp }
}

/// The main action of a screen (solid accent).
pub fn primary_button(ui: &mut Ui, text: &str) -> Response {
    kit_button(ui, text, ButtonLook { idle: theme::PRIMARY, hover: theme::PRIMARY_HOVER, active: theme::PRIMARY_ACTIVE, stroke: Stroke::NONE, text: theme::ON_ACCENT }, None)
}

/// Outlined destructive button.
pub fn danger_button(ui: &mut Ui, text: &str) -> Response {
    kit_button(
        ui,
        text,
        ButtonLook {
            idle: Color32::TRANSPARENT,
            hover: theme::ERROR.gamma_multiply(0.18),
            active: theme::ERROR.gamma_multiply(0.30),
            stroke: Stroke::new(1.0_f32, theme::ERROR.gamma_multiply(0.6)),
            text: theme::ERROR,
        },
        None,
    )
}

/// Secondary button (flat card-coloured).
pub fn secondary_button(ui: &mut Ui, text: &str) -> Response {
    kit_button(ui, text, ButtonLook { idle: theme::FIELD_ROW_BG, hover: theme::INPUT_FILL_BLUE, active: theme::PRIMARY.gamma_multiply(0.30), stroke: Stroke::new(1.0_f32, theme::BUTTON_BORDER), text: theme::TEXT }, None)
}

/// Frameless icon button; the tooltip is mandatory.
pub fn ico_button(ui: &mut Ui, ico: Ico, tip: &str) -> Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(ICON_BUTTON), Sense::click());
    let enabled = ui.is_enabled();
    let color = if !enabled {
        theme::LABEL.gamma_multiply(0.4)
    } else if resp.hovered() {
        Color32::WHITE
    } else {
        theme::LABEL
    };
    if resp.hovered() && enabled {
        ui.painter().rect_filled(rect, theme::RADIUS_CTRL, theme::INPUT_FILL_BLUE);
    }
    glyphs::paint(ui.painter(), rect, ico, color);
    let resp = resp.on_hover_text(tip);
    if enabled { resp.on_hover_cursor(CursorIcon::PointingHand) } else { resp }
}

/// Bordered card.
pub fn card<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> InnerResponse<R> {
    Frame::new()
        .fill(theme::FIELD_ROW_BG)
        .stroke(Stroke::new(1.0_f32, theme::FIELD_ROW_BORDER))
        .corner_radius(theme::RADIUS_CARD)
        .inner_margin(Margin::symmetric(16, 12))
        .show(ui, |ui| {
            // pin the width so children can never make the card wider than its siblings
            let w = ui.available_width();
            ui.set_min_width(w);
            ui.set_max_width(w);
            add(ui)
        })
}

/// Single line text input filling the width.
pub fn input(ui: &mut Ui, text: &mut String, hint: &str) -> Response {
    ui.add(TextEdit::singleline(text).hint_text(hint).margin(INPUT_MARGIN).desired_width(f32::INFINITY))
}

/// Multi line text input.
pub fn text_area(ui: &mut Ui, text: &mut String, rows: usize) -> Response {
    ui.add(TextEdit::multiline(text).margin(INPUT_MARGIN).desired_rows(rows).desired_width(f32::INFINITY))
}

/// Masked input with an eye toggle.
pub fn secret_input(ui: &mut Ui, text: &mut String, reveal: &mut bool) -> Response {
    ui.horizontal(|ui| {
        // desired_width excludes the inner margin, which is added on both sides
        let margins = f32::from(INPUT_MARGIN.left) + f32::from(INPUT_MARGIN.right);
        let width = (ui.available_width() - ICON_BUTTON - ui.spacing().item_spacing.x - margins).max(40.0);
        let resp = ui.add(TextEdit::singleline(text).password(!*reveal).margin(INPUT_MARGIN).desired_width(width));
        if ico_button(ui, if *reveal { Ico::EyeOff } else { Ico::Eye }, if *reveal { "Hide" } else { "Show" }).clicked() {
            *reveal = !*reveal;
        }
        resp
    })
    .inner
}

/// Search box with a magnifier and a clear button.
pub fn search_box(ui: &mut Ui, text: &mut String, hint: &str) -> Response {
    let margin = Margin { left: 34, right: 30, top: 9, bottom: 9 };
    let resp = ui.add(TextEdit::singleline(text).hint_text(hint).margin(margin).desired_width(f32::INFINITY));
    // the response rect excludes the inner margin, so offset from the frame edges
    let rect = resp.rect;
    let icon_rect = egui::Rect::from_center_size(egui::pos2(rect.left() - f32::from(margin.left) + 17.0, rect.center().y), Vec2::splat(ICON_BUTTON));
    glyphs::paint(ui.painter(), icon_rect, Ico::Search, if resp.has_focus() { theme::SECONDARY } else { theme::LABEL });
    if !text.is_empty() {
        let clear = egui::Rect::from_center_size(egui::pos2(rect.right() + f32::from(margin.right) - 16.0, rect.center().y), Vec2::splat(ICON_BUTTON));
        let r = ui.interact(clear, resp.id.with("clear"), Sense::click());
        glyphs::paint(ui.painter(), clear, Ico::Close, if r.hovered() { Color32::WHITE } else { theme::LABEL });
        if r.on_hover_text("Clear").on_hover_cursor(CursorIcon::PointingHand).clicked() {
            text.clear();
        }
    }
    resp
}

/// Removable chip (active filter, tag).
pub fn removable_chip(ui: &mut Ui, text: &str, tip: &str) -> bool {
    let mut removed = false;
    Frame::new()
        .fill(theme::INPUT_FILL_BLUE)
        .stroke(Stroke::new(1.0_f32, theme::PRIMARY.gamma_multiply(0.35)))
        .corner_radius(theme::RADIUS_CTRL)
        .inner_margin(Margin::symmetric(8, 2))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                ui.label(RichText::new(text).size(12.0).color(theme::SECONDARY));
                let (rect, resp) = ui.allocate_exact_size(Vec2::splat(16.0), Sense::click());
                glyphs::paint(ui.painter(), rect, Ico::Close, if resp.hovered() { Color32::WHITE } else { theme::SECONDARY });
                removed = resp.on_hover_text(tip).on_hover_cursor(CursorIcon::PointingHand).clicked();
            });
        });
    removed
}

/// Rounded tinted square holding an icon.
pub fn icon_badge(ui: &mut Ui, ico: Ico, color: Color32, size: f32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    paint_badge(ui, rect, color, size);
    glyphs::paint(ui.painter(), rect.shrink(size * 0.0), ico, color);
}

/// Rounded tinted square holding a short text monogram.
pub fn text_badge(ui: &mut Ui, text: &str, color: Color32, size: f32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    paint_badge(ui, rect, color, size);
    let font = FontId::new(size * 0.38, theme::semibold());
    ui.painter().text(rect.center(), Align2::CENTER_CENTER, text, font, color);
}

/// Background of badges.
fn paint_badge(ui: &Ui, rect: egui::Rect, color: Color32, size: f32) {
    let radius = (size * 0.22).round() as u8;
    ui.painter().rect_filled(rect, radius, color.gamma_multiply(0.16));
    ui.painter().rect_stroke(rect, radius, Stroke::new(1.0_f32, color.gamma_multiply(0.45)), StrokeKind::Inside);
}

/// Small coloured status dot with a tooltip.
pub fn status_dot(ui: &mut Ui, color: Color32, tip: &str) -> Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(12.0), Sense::hover());
    ui.painter().circle_filled(rect.center(), 4.0, color);
    resp.on_hover_text(tip)
}

/// Navigation rail tile.
pub fn rail_tile(ui: &mut Ui, ico: Ico, active: bool, tip: &str) -> Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(RAIL_TILE), Sense::click());
    let hovered = resp.hovered();
    if active {
        ui.painter().rect_filled(rect, theme::RADIUS_CARD, theme::INPUT_FILL_BLUE);
        let bar = egui::Rect::from_min_size(egui::pos2(rect.left() - 8.0, rect.top() + 8.0), egui::vec2(3.0, rect.height() - 16.0));
        ui.painter().rect_filled(bar, 1.0, theme::PRIMARY);
    } else if hovered {
        ui.painter().rect_filled(rect, theme::RADIUS_CARD, theme::FIELD_ROW_BG);
    }
    let color = if active { theme::SECONDARY } else if hovered { theme::TEXT } else { theme::LABEL };
    glyphs::paint(ui.painter(), rect, ico, color);
    resp.on_hover_text(tip).on_hover_cursor(CursorIcon::PointingHand)
}

/// Makes menu rows flat (transparent until hovered).
pub fn flat_menu_style(ui: &mut Ui) {
    let v = ui.visuals_mut();
    v.widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
    v.widgets.inactive.bg_fill = Color32::TRANSPARENT;
    v.widgets.inactive.bg_stroke = Stroke::NONE;
    ui.spacing_mut().button_padding = egui::vec2(10.0, 6.0);
}

/// Centered modal window.
pub fn modal<R>(ctx: &egui::Context, title: &str, add: impl FnOnce(&mut Ui) -> R) -> Option<InnerResponse<Option<R>>> {
    egui::Window::new(semibold(title, 15.0))
        .collapsible(false)
        .resizable(false)
        .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
        .min_width(360.0)
        .show(ctx, add)
}

/// Draws the toast stack.
pub fn draw_toasts(ctx: &egui::Context, toasts: &mut Toasts) {
    toasts.prune();
    if toasts.items.is_empty() {
        return;
    }
    Area::new(egui::Id::new("toasts")).anchor(Align2::RIGHT_BOTTOM, [-16.0, -16.0]).order(Order::Foreground).show(ctx, |ui| {
        ui.with_layout(Layout::bottom_up(Align::Max), |ui| {
            for t in toasts.items.iter().rev() {
                let bar = match t.kind {
                    ToastKind::Ok => theme::OK,
                    ToastKind::Error => theme::ERROR,
                    ToastKind::Info => theme::PRIMARY,
                };
                Frame::new()
                    .fill(theme::SURFACE)
                    .stroke(Stroke::new(1.0_f32, theme::FIELD_ROW_BORDER))
                    .corner_radius(theme::RADIUS_CARD)
                    .shadow(Shadow { offset: [0, 6], blur: 18, spread: 0, color: Color32::from_black_alpha(140) })
                    .inner_margin(Margin::symmetric(14, 10))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            let (rect, _) = ui.allocate_exact_size(egui::vec2(3.0, 18.0), Sense::hover());
                            ui.painter().rect_filled(rect, 1.0, bar);
                            ui.label(RichText::new(&t.text).color(theme::TEXT));
                        });
                    });
            }
        });
    });
    ctx.request_repaint_after(Duration::from_millis(500));
}
