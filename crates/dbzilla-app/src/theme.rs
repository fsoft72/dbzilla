//! "Obsidian" design system: near-black neutral surfaces, one electric-blue
//! accent, sharp corners (3-6 px) and IBM Plex typography.


use eframe::egui::{self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, Stroke};
use std::sync::Arc;

pub const DEEP_DARK: Color32 = Color32::from_rgb(0x0E, 0x11, 0x17);
pub const NAVY_DARK: Color32 = Color32::from_rgb(0x09, 0x0B, 0x0F);
pub const INPUT_FILL: Color32 = Color32::from_rgb(0x0A, 0x0D, 0x12);
pub const INPUT_FILL_BLUE: Color32 = Color32::from_rgb(0x17, 0x20, 0x38);
pub const FIELD_ROW_BG: Color32 = Color32::from_rgb(0x14, 0x18, 0x21);
pub const FIELD_ROW_BORDER: Color32 = Color32::from_rgb(0x25, 0x2B, 0x3A);
pub const PRIMARY: Color32 = Color32::from_rgb(0x3F, 0x6B, 0xDB);
pub const SECONDARY: Color32 = Color32::from_rgb(0x8F, 0xB1, 0xF7);
pub const TERTIARY: Color32 = Color32::from_rgb(0xF2, 0xB8, 0x4B);
pub const SURFACE: Color32 = Color32::from_rgb(0x13, 0x17, 0x20);
pub const ERROR: Color32 = Color32::from_rgb(0xF0, 0x70, 0x7F);
pub const OK: Color32 = Color32::from_rgb(0x3D, 0xD6, 0x8C);
pub const LABEL: Color32 = Color32::from_rgb(0x8A, 0x94, 0xA9);
pub const MUTED: Color32 = Color32::from_rgb(0x80, 0x89, 0x9E);
pub const TEXT: Color32 = Color32::from_rgb(0xE6, 0xEA, 0xF2);
pub const ON_ACCENT: Color32 = Color32::WHITE;
pub const PRIMARY_HOVER: Color32 = Color32::from_rgb(0x4F, 0x7B, 0xE8);
pub const PRIMARY_ACTIVE: Color32 = Color32::from_rgb(0x34, 0x59, 0xC0);

/// Idle button border, slightly lighter than `FIELD_ROW_BORDER`.
pub const BUTTON_BORDER: Color32 = Color32::from_rgb(0x32, 0x3A, 0x4D);

/// Colours of the table groups in the ER diagram: tables linked by foreign keys share one.
/// They reuse the semantic colours of the theme, so the diagram adds no new hue.
pub const DIAGRAM_GROUPS: [Color32; 5] = [PRIMARY, OK, TERTIARY, SECONDARY, ERROR];

pub const RADIUS_CTRL: u8 = 3;
pub const RADIUS_CARD: u8 = 4;
pub const RADIUS_WINDOW: u8 = 6;

/// Font family of semi-bold text (headings, brand, buttons).
pub fn semibold() -> FontFamily {
    FontFamily::Name("semibold".into())
}

/// Registers IBM Plex Sans (regular, medium, semi-bold) and Plex Mono. The
/// egui default fonts stay as fallback, so emoji and symbols still render.
fn install_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    let faces: [(&str, &[u8]); 4] = [
        ("plex-sans", include_bytes!("../assets/fonts/IBMPlexSans-Regular.ttf")),
        ("plex-sans-medium", include_bytes!("../assets/fonts/IBMPlexSans-Medium.ttf")),
        ("plex-sans-semibold", include_bytes!("../assets/fonts/IBMPlexSans-SemiBold.ttf")),
        ("plex-mono", include_bytes!("../assets/fonts/IBMPlexMono-Regular.ttf")),
    ];
    for (name, data) in faces {
        fonts.font_data.insert(name.to_owned(), Arc::new(FontData::from_static(data)));
    }
    let fallback = fonts.families[&FontFamily::Proportional].clone();
    let mono_fallback = fonts.families[&FontFamily::Monospace].clone();
    let with = |first: &str, rest: &[String]| {
        let mut v = vec![first.to_owned()];
        v.extend(rest.iter().cloned());
        v
    };
    fonts.families.insert(FontFamily::Proportional, with("plex-sans", &fallback));
    fonts.families.insert(FontFamily::Monospace, with("plex-mono", &mono_fallback));
    fonts.families.insert(semibold(), with("plex-sans-semibold", &fallback));
    fonts.families.insert(FontFamily::Name("medium".into()), with("plex-sans-medium", &fallback));
    ctx.set_fonts(fonts);
}

/// Installs fonts, colours, radii and spacing on `ctx`.
pub fn apply(ctx: &egui::Context) {
    install_fonts(ctx);
    let mut v = egui::Visuals::dark();
    v.panel_fill = DEEP_DARK;
    v.window_fill = SURFACE;
    v.extreme_bg_color = INPUT_FILL;      // text input background
    v.faint_bg_color = FIELD_ROW_BG;
    v.code_bg_color = INPUT_FILL;
    v.hyperlink_color = SECONDARY;
    v.warn_fg_color = TERTIARY;
    v.error_fg_color = ERROR;
    v.selection.bg_fill = PRIMARY.gamma_multiply(0.35);
    v.selection.stroke = Stroke::new(1.0_f32, SECONDARY);
    v.window_corner_radius = CornerRadius::same(RADIUS_WINDOW);
    v.menu_corner_radius = CornerRadius::same(RADIUS_WINDOW);
    v.window_stroke = Stroke::new(1.0_f32, FIELD_ROW_BORDER);
    v.window_shadow = egui::Shadow { offset: [0, 12], blur: 32, spread: 0, color: Color32::from_black_alpha(160) };
    v.popup_shadow = v.window_shadow;
    v.striped = false;

    let r = CornerRadius::same(RADIUS_CTRL);
    let w = &mut v.widgets;
    for s in [&mut w.noninteractive, &mut w.inactive, &mut w.hovered, &mut w.active, &mut w.open] {
        s.corner_radius = r;
    }
    w.noninteractive.bg_fill = FIELD_ROW_BG;
    w.noninteractive.bg_stroke = Stroke::new(1.0_f32, FIELD_ROW_BORDER);
    w.noninteractive.fg_stroke = Stroke::new(1.0_f32, TEXT);
    // Buttons: flat, hairline border, accent border on hover.
    w.inactive.bg_fill = FIELD_ROW_BG;
    w.inactive.weak_bg_fill = FIELD_ROW_BG;
    w.inactive.bg_stroke = Stroke::new(1.0_f32, BUTTON_BORDER);
    w.inactive.fg_stroke = Stroke::new(1.0_f32, TEXT);
    w.hovered.bg_fill = INPUT_FILL_BLUE;
    w.hovered.weak_bg_fill = INPUT_FILL_BLUE;
    w.hovered.bg_stroke = Stroke::new(1.0_f32, PRIMARY);
    w.hovered.fg_stroke = Stroke::new(1.0_f32, Color32::WHITE);
    w.hovered.expansion = 0.0;            // no grow-on-hover: the look is flat
    w.active.bg_fill = PRIMARY.gamma_multiply(0.30);
    w.active.weak_bg_fill = PRIMARY.gamma_multiply(0.30);
    w.active.bg_stroke = Stroke::new(1.0_f32, SECONDARY);
    w.active.fg_stroke = Stroke::new(1.0_f32, Color32::WHITE);
    w.active.expansion = 0.0;
    w.open.bg_fill = INPUT_FILL_BLUE;
    w.open.weak_bg_fill = INPUT_FILL_BLUE;
    w.open.bg_stroke = Stroke::new(1.0_f32, PRIMARY);
    ctx.set_visuals(v);

    ctx.style_mut(|s| {
        s.spacing.item_spacing = egui::vec2(8.0, 8.0);
        s.spacing.button_padding = egui::vec2(14.0, 6.0);
        s.spacing.interact_size.y = 32.0;
        s.spacing.menu_margin = egui::Margin::same(6);
        s.spacing.scroll = egui::style::ScrollStyle::floating();
        s.interaction.selectable_labels = false;
        use egui::{FontId, TextStyle};
        s.text_styles = [
            (TextStyle::Heading, FontId::new(22.0, semibold())),
            (TextStyle::Body, FontId::new(14.0, FontFamily::Proportional)),
            (TextStyle::Button, FontId::new(13.5, FontFamily::Name("medium".into()))),
            (TextStyle::Small, FontId::new(12.0, FontFamily::Proportional)),
            (TextStyle::Monospace, FontId::new(14.0, FontFamily::Monospace)),
        ]
        .into();
    });
}

/// "RRGGBB" / "#RRGGBB" / "AARRGGBB" to an opaque colour (user-chosen colours).
pub fn parse_hex_color(s: &str) -> Option<Color32> {
    let h = s.trim().trim_start_matches('#');
    let v = u32::from_str_radix(h, 16).ok()?;
    match h.len() {
        6 | 8 => Some(Color32::from_rgb((v >> 16) as u8, (v >> 8) as u8, v as u8)),
        _ => None,
    }
}

/// Opaque colour as "RRGGBB".
#[cfg(test)]
pub fn to_hex(c: Color32) -> String {
    format!("{:02X}{:02X}{:02X}", c.r(), c.g(), c.b())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Accepts hex with and without the leading hash.
    #[test]
    fn parse_hex_color_accepts_hash_and_plain() {
        assert_eq!(parse_hex_color("#3F6BDB"), Some(PRIMARY));
        assert_eq!(parse_hex_color("3F6BDB"), Some(PRIMARY));
    }

    /// Rejects garbage and wrong lengths.
    #[test]
    fn parse_hex_color_rejects_invalid() {
        assert_eq!(parse_hex_color("zz"), None);
        assert_eq!(parse_hex_color("FFF"), None);
    }

    /// `to_hex` is the inverse of `parse_hex_color`.
    #[test]
    fn hex_round_trip() {
        assert_eq!(parse_hex_color(&to_hex(SECONDARY)), Some(SECONDARY));
    }
}
