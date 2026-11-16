//! MCP settings tab (server, tokens, activity) and the approval dialog (SPEC 16).

use super::widgets;
use crate::mcpbackend::McpHost;
use crate::theme;
use dbzilla_mcp::perm::Permission;
use dbzilla_store::settings::Settings;
use eframe::egui::{self, RichText, Ui};

/// Lines of the activity log shown.
const ACTIVITY_SHOWN: usize = 30;

/// Draws the MCP tab; returns true when a setting changed.
pub fn settings_tab(ui: &mut Ui, s: &mut Settings, host: &mut McpHost) -> bool {
    let before = s.mcp.clone();
    ui.checkbox(&mut s.mcp.enabled, "Enable the MCP server");
    ui.horizontal(|ui| {
        ui.label("Port");
        let mut port = s.mcp.port.to_string();
        if ui.add(egui::TextEdit::singleline(&mut port).desired_width(70.0)).changed() {
            if let Ok(p) = port.parse::<u16>() {
                s.mcp.port = p;
            }
        }
    });
    ui.checkbox(&mut s.mcp.approval_needed, "Ask me before a client runs a write or schema change");
    let (dot, text) = match (&host.error, host.running()) {
        (Some(e), _) => (theme::ERROR, e.clone()),
        (None, true) => (theme::OK, format!("Listening on http://127.0.0.1:{}/mcp", s.mcp.port)),
        (None, false) => (theme::MUTED, "Stopped".to_owned()),
    };
    ui.horizontal(|ui| {
        widgets::status_dot(ui, dot, "");
        ui.label(RichText::new(text).size(12.5).color(theme::MUTED));
    });
    ui.add_space(10.0);
    widgets::section_title(ui, "Tokens");
    if let Some(secret) = host.new_secret.clone() {
        widgets::card(ui, |ui| {
            ui.label("Copy this token now, it is not shown again:");
            ui.label(RichText::new(&secret).monospace().color(theme::SECONDARY));
            if widgets::secondary_button(ui, "Copy").clicked() {
                ui.ctx().copy_text(secret);
            }
            if widgets::secondary_button(ui, "Done").clicked() {
                host.new_secret = None;
            }
        });
    }
    if let Some(state) = host.state.clone() {
        let mut revoke = None;
        if let Ok(t) = state.tokens.lock() {
            for tok in &t.tokens {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(&tok.name).size(13.0));
                    ui.label(RichText::new(tok.permission.label()).size(12.0).color(theme::MUTED));
                    if ui.small_button("Revoke").clicked() {
                        revoke = Some(tok.id.clone());
                    }
                });
            }
        }
        if let Some(id) = revoke {
            if let Ok(mut t) = state.tokens.lock() {
                if let Err(e) = t.revoke(&id) {
                    host.error = Some(e);
                }
            }
        }
        ui.horizontal(|ui| {
            let w = 160.0;
            ui.allocate_ui(egui::vec2(w, 32.0), |ui| widgets::input(ui, &mut host.new_name, "token name"));
            egui::ComboBox::from_id_salt("mcp_perm").selected_text(host.new_perm.label()).show_ui(ui, |ui| {
                for p in [Permission::ReadOnly, Permission::ReadWrite, Permission::Full] {
                    ui.selectable_value(&mut host.new_perm, p, p.label());
                }
            });
            if widgets::primary_button(ui, "Create token").clicked() && !host.new_name.trim().is_empty() {
                let name = host.new_name.trim().to_owned();
                let perm = host.new_perm;
                match state.tokens.lock().map_err(|e| e.to_string()).and_then(|mut t| t.create(&name, perm)) {
                    Ok(secret) => {
                        host.new_secret = Some(secret);
                        host.new_name.clear();
                    }
                    Err(e) => host.error = Some(e),
                }
            }
        });
    } else {
        widgets::muted(ui, "Enable the server to manage tokens.");
    }
    ui.add_space(10.0);
    widgets::section_title(ui, "Activity");
    let log = host.shared.activity.lock().map(|a| a.clone()).unwrap_or_default();
    if log.is_empty() {
        widgets::muted(ui, "No client activity yet.");
    }
    egui::ScrollArea::vertical().max_height(180.0).show(ui, |ui| {
        for a in log.iter().take(ACTIVITY_SHOWN) {
            let color = if a.outcome == "ok" { theme::MUTED } else { theme::ERROR };
            ui.label(RichText::new(format!("{}  {}  {}  {}", a.token, a.tool, a.detail, a.outcome)).size(12.0).color(color));
        }
    });
    s.mcp != before
}

/// Shows the oldest write waiting for approval.
pub fn approval_modal(ctx: &egui::Context, host: &mut McpHost) {
    let Some(first) = host.pending.first() else { return };
    let (token, conn, sql, kind) = (first.req.token.clone(), first.req.connection.clone(), first.req.sql.clone(), first.req.kind);
    let mut decision = None;
    widgets::modal(ctx, "An MCP client wants to change data", |ui| {
        ui.set_width(460.0);
        ui.label(format!("Token {token} asks to run a {kind:?} statement on {conn}:"));
        egui::ScrollArea::vertical().max_height(180.0).show(ui, |ui| {
            ui.label(RichText::new(&sql).monospace().size(12.5));
        });
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if widgets::primary_button(ui, "Approve").clicked() {
                decision = Some(true);
            }
            if widgets::danger_button(ui, "Deny").clicked() {
                decision = Some(false);
            }
        });
    });
    if let Some(ok) = decision {
        let p = host.pending.remove(0);
        let _ = p.answer.send(ok);
    }
}
