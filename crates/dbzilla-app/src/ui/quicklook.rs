//! Quick Look: inspect one cell as text, JSON or hex (SPEC 7.8).

use super::widgets;
use crate::theme;
use dbzilla_core::value::Value;
use eframe::egui::{self, FontId, Key, RichText, Ui};

/// Largest number of bytes shown in the hex view.
const MAX_HEX_BYTES: usize = 64 * 1024;
/// Bytes per hex row.
const HEX_ROW: usize = 16;

/// How the value is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QlMode {
    Text,
    JsonRaw,
    JsonTree,
    Hex,
}

/// An open Quick Look window.
pub struct QuickLook {
    pub column: String,
    pub value: Value,
    pub mode: QlMode,
    pub pretty: bool,
}

impl QuickLook {
    /// Opens a view with the best mode for the value.
    pub fn new(column: &str, value: Value) -> Self {
        let mode = match &value {
            Value::Json(_) => QlMode::JsonTree,
            Value::Bytes(_) => QlMode::Hex,
            Value::Text(s) if parse_json(s).is_some() => QlMode::Text,
            _ => QlMode::Text,
        };
        Self { column: column.to_owned(), value, mode, pretty: true }
    }

    /// The value as JSON when it is JSON or text that parses as JSON.
    pub fn json(&self) -> Option<serde_json::Value> {
        match &self.value {
            Value::Json(j) => Some(j.clone()),
            Value::Text(s) => parse_json(s),
            _ => None,
        }
    }
}

/// Parses text as a JSON object or array only (plain numbers and strings are not offered as JSON).
fn parse_json(s: &str) -> Option<serde_json::Value> {
    let t = s.trim_start();
    if !(t.starts_with('{') || t.starts_with('[')) {
        return None;
    }
    serde_json::from_str(s).ok()
}

/// Classic hex dump: offset, bytes and printable characters.
pub fn hex_dump(bytes: &[u8]) -> String {
    let shown = &bytes[..bytes.len().min(MAX_HEX_BYTES)];
    let mut out = String::new();
    for (i, chunk) in shown.chunks(HEX_ROW).enumerate() {
        let hex: Vec<String> = chunk.iter().map(|b| format!("{b:02x}")).collect();
        let ascii: String = chunk.iter().map(|b| if (0x20..0x7f).contains(b) { *b as char } else { '.' }).collect();
        out.push_str(&format!("{:08x}  {:<47}  {ascii}\n", i * HEX_ROW, hex.join(" ")));
    }
    if bytes.len() > shown.len() {
        out.push_str(&format!("... {} more bytes not shown\n", bytes.len() - shown.len()));
    }
    out
}

/// Text of a value with JSON pretty-printing or minifying when applicable.
pub fn text_of(v: &Value, json_pretty: bool) -> String {
    match v {
        Value::Json(j) => if json_pretty { serde_json::to_string_pretty(j) } else { serde_json::to_string(j) }.unwrap_or_default(),
        other => other.to_display(),
    }
}

/// Draws the window; returns false when it should close.
pub fn show(ctx: &egui::Context, ql: &mut QuickLook) -> bool {
    let mut open = true;
    if ctx.input(|i| i.key_pressed(Key::Escape)) {
        open = false;
    }
    egui::Window::new(widgets::semibold(format!("Quick look: {}", ql.column), 14.0)).open(&mut open).collapsible(false).default_size([640.0, 420.0]).resizable(true).show(ctx, |ui| {
        let json = ql.json();
        ui.horizontal(|ui| {
            ui.selectable_value(&mut ql.mode, QlMode::Text, "Text");
            if json.is_some() {
                ui.selectable_value(&mut ql.mode, QlMode::JsonTree, "JSON tree");
                ui.selectable_value(&mut ql.mode, QlMode::JsonRaw, "JSON raw");
            }
            if matches!(ql.value, Value::Bytes(_)) {
                ui.selectable_value(&mut ql.mode, QlMode::Hex, "Hex");
            }
            if matches!(ql.mode, QlMode::JsonRaw | QlMode::Text) && json.is_some() {
                ui.checkbox(&mut ql.pretty, "Pretty");
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if widgets::secondary_button(ui, "Copy").clicked() {
                    ui.ctx().copy_text(body_text(ql, json.as_ref()));
                }
            });
        });
        ui.separator();
        egui::ScrollArea::both().auto_shrink([false, false]).show(ui, |ui| match (ql.mode, &json) {
            (QlMode::JsonTree, Some(j)) => json_tree(ui, "root", j, true),
            (QlMode::Hex, _) => {
                if let Value::Bytes(b) = &ql.value {
                    ui.add(egui::Label::new(RichText::new(hex_dump(b)).font(FontId::monospace(12.5)).color(theme::TEXT)).selectable(true));
                }
            }
            _ => {
                ui.add(egui::Label::new(RichText::new(body_text(ql, json.as_ref())).font(FontId::monospace(13.0)).color(theme::TEXT)).selectable(true));
            }
        });
    });
    open
}

/// Text shown (and copied) for the current mode.
fn body_text(ql: &QuickLook, json: Option<&serde_json::Value>) -> String {
    match (ql.mode, json) {
        (QlMode::JsonRaw | QlMode::JsonTree, Some(j)) | (QlMode::Text, Some(j)) if ql.pretty && !matches!(ql.value, Value::Text(_)) || ql.mode == QlMode::JsonRaw => {
            if ql.pretty { serde_json::to_string_pretty(j) } else { serde_json::to_string(j) }.unwrap_or_default()
        }
        (QlMode::Hex, _) => match &ql.value {
            Value::Bytes(b) => hex_dump(b),
            other => other.to_display(),
        },
        _ => text_of(&ql.value, ql.pretty),
    }
}

/// Recursive collapsible JSON tree.
fn json_tree(ui: &mut Ui, key: &str, v: &serde_json::Value, open: bool) {
    match v {
        serde_json::Value::Object(map) => {
            egui::CollapsingHeader::new(RichText::new(format!("{key}  {{{}}}", map.len())).color(theme::SECONDARY)).default_open(open).id_salt(ui.next_auto_id()).show(ui, |ui| {
                for (k, v) in map {
                    json_tree(ui, k, v, false);
                }
            });
        }
        serde_json::Value::Array(items) => {
            egui::CollapsingHeader::new(RichText::new(format!("{key}  [{}]", items.len())).color(theme::SECONDARY)).default_open(open).id_salt(ui.next_auto_id()).show(ui, |ui| {
                for (i, v) in items.iter().enumerate() {
                    json_tree(ui, &i.to_string(), v, false);
                }
            });
        }
        scalar => {
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("{key}:")).color(theme::MUTED));
                ui.label(RichText::new(scalar.to_string()).font(FontId::monospace(13.0)).color(theme::TEXT));
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hex rows show offset, hex bytes and printable text.
    #[test]
    fn hex_dump_format() {
        let d = hex_dump(b"Hello, World!\x00\xff");
        assert!(d.starts_with("00000000  48 65 6c 6c 6f 2c 20 57 6f 72 6c 64 21 00 ff"), "{d}");
        assert!(d.trim_end().ends_with("Hello, World!.."), "{d}");
        assert_eq!(hex_dump(&[]), "");
    }

    /// Big blobs are cut with a note.
    #[test]
    fn hex_dump_is_capped() {
        let d = hex_dump(&vec![0u8; MAX_HEX_BYTES + 100]);
        assert!(d.ends_with("... 100 more bytes not shown\n"));
    }

    /// JSON values and JSON looking text are detected; plain numbers are not.
    #[test]
    fn json_detection() {
        assert!(QuickLook::new("c", Value::Json(serde_json::json!({"a": 1}))).json().is_some());
        assert!(QuickLook::new("c", Value::Text("{\"a\": [1,2]}".into())).json().is_some());
        assert!(QuickLook::new("c", Value::Text("12".into())).json().is_none());
        assert!(QuickLook::new("c", Value::Text("{broken".into())).json().is_none());
        assert_eq!(QuickLook::new("c", Value::Bytes(vec![1])).mode, QlMode::Hex);
        assert_eq!(QuickLook::new("c", Value::Json(serde_json::json!([]))).mode, QlMode::JsonTree);
    }

    /// Pretty and minified JSON text.
    #[test]
    fn json_text_modes() {
        let v = Value::Json(serde_json::json!({"a": 1}));
        assert_eq!(text_of(&v, false), "{\"a\":1}");
        assert!(text_of(&v, true).contains('\n'));
    }
}
