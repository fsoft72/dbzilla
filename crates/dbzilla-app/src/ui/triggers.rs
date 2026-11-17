//! Trigger editor (SPEC 8.8): a list of the triggers of a table and a form to add, change or delete one.
//! Changing a trigger is a drop followed by a create, because neither engine can alter one in place.

use super::tabs::TabId;
use super::widgets::{self, muted};
use crate::theme;
use dbzilla_core::ddl::{generate_ddl, StructuralOp, TriggerDef, TriggerLevel};
use dbzilla_core::dialect::{dialect_for, DriverId};
use dbzilla_core::model::{TableRef, TriggerInfo};
use eframe::egui::{self, RichText};

/// Timings the form offers.
const TIMINGS: [&str; 2] = ["BEFORE", "AFTER"];
/// Events the form offers.
const EVENTS: [&str; 3] = ["INSERT", "UPDATE", "DELETE"];
/// Why a trigger cannot go through the form.
const NOT_EDITABLE: &str = "This trigger uses options the editor cannot keep (several events, UPDATE OF, WHEN, arguments, transition tables, INSTEAD OF, TRUNCATE, a constraint trigger or a disabled trigger). Edit it in a SQL tab.";

/// A trigger being created or changed.
#[derive(Debug, Clone, PartialEq)]
pub struct TriggerDraft {
    /// The trigger as it is in the database; `None` for a new one.
    pub original: Option<TriggerInfo>,
    pub driver: DriverId,
    pub name: String,
    pub timing: String,
    pub event: String,
    pub level: TriggerLevel,
    /// PostgreSQL: the function to call.
    pub function: Option<TableRef>,
    /// MySQL: the statement or `BEGIN ... END` block.
    pub body: String,
    /// False when the original has options the form cannot represent.
    pub editable: bool,
    /// The name this draft was last sent to the server as, so the draft can follow it once the list is read again.
    pub submitted: Option<String>,
}

impl TriggerDraft {
    /// An empty draft for a new trigger.
    pub fn blank(driver: DriverId) -> Self {
        Self { original: None, driver, name: String::new(), timing: TIMINGS[0].into(), event: EVENTS[0].into(), level: TriggerLevel::Row, function: None, body: String::new(), editable: true, submitted: None }
    }

    /// A draft that starts from an existing trigger.
    pub fn from_info(info: &TriggerInfo, driver: DriverId) -> Self {
        let level = if info.level == "STATEMENT" { TriggerLevel::Statement } else { TriggerLevel::Row };
        let body = if driver == DriverId::MySql { info.statement.clone() } else { String::new() };
        Self { original: Some(info.clone()), driver, name: info.name.clone(), timing: info.timing.clone(), event: info.event.clone(), level, function: info.function.clone(), body, editable: info.editable, submitted: None }
    }

    /// Follows the list after it was read again. Once the original is gone, the draft becomes the trigger it was
    /// saved as (a rename, or a first save), or a new one when the server lost it (a failed MySQL edit), keeping
    /// the text typed. While the original is still listed nothing changes: the reload has not arrived yet.
    pub fn sync(&mut self, triggers: &[TriggerInfo]) {
        if self.original.as_ref().is_some_and(|o| triggers.iter().any(|t| t.name == o.name)) {
            return;
        }
        let name = self.name.trim();
        self.original = self.submitted.as_deref().filter(|n| *n == name).and_then(|n| triggers.iter().find(|t| t.name == n).cloned());
    }

    /// The statements that save the draft: a create, preceded by a drop of the original when there is one.
    /// The message is the reason when the draft cannot be saved.
    pub fn to_ops(&self, table: &TableRef) -> Result<Vec<StructuralOp>, String> {
        if !self.editable {
            return Err(NOT_EDITABLE.into());
        }
        let trigger = TriggerDef { name: self.name.trim().to_owned(), timing: self.timing.clone(), event: self.event.clone(), level: self.level, function: self.function.clone(), body: self.body.clone() };
        let create = StructuralOp::CreateTrigger { table: table.clone(), trigger };
        generate_ddl(&create, dialect_for(self.driver).as_ref()).map_err(|e| e.to_string())?;
        let mut ops = Vec::new();
        if let Some(o) = &self.original {
            ops.push(StructuralOp::DropTrigger { table: table.clone(), name: o.name.clone(), if_exists: true });
        }
        ops.push(create);
        Ok(ops)
    }
}

/// The statements that delete a trigger.
pub fn delete_ops(table: &TableRef, name: &str) -> Vec<StructuralOp> {
    vec![StructuralOp::DropTrigger { table: table.clone(), name: name.to_owned(), if_exists: false }]
}

/// State of the trigger window.
pub struct TriggerEditor {
    pub tab: TabId,
    pub table: TableRef,
    draft: Option<TriggerDraft>,
    selected: Option<usize>,
    confirm_delete: bool,
    error: Option<String>,
}

/// What the window asks of the workspace.
#[derive(Debug, Clone, PartialEq)]
pub enum TriggerAction {
    /// Run these statements, then show the message.
    Submit { ops: Vec<StructuralOp>, message: String },
    Close,
}

/// What the window reads from the workspace.
pub struct TriggerEnv<'a> {
    pub driver: DriverId,
    pub triggers: &'a [TriggerInfo],
    /// PostgreSQL trigger functions.
    pub functions: &'a [TableRef],
}

impl TriggerEditor {
    /// A window on the triggers of `table`, nothing selected.
    pub fn new(tab: TabId, table: TableRef) -> Self {
        Self { tab, table, draft: None, selected: None, confirm_delete: false, error: None }
    }

    /// Starts a new trigger (`Mod+I`, `+ Trigger`).
    pub fn start_new(&mut self, driver: DriverId) {
        if driver == DriverId::Sqlite {
            return;
        }
        self.selected = None;
        self.confirm_delete = false;
        self.error = None;
        self.draft = Some(TriggerDraft::blank(driver));
    }

    /// The draft being edited (tests).
    #[cfg(test)]
    pub fn draft(&self) -> Option<&TriggerDraft> {
        self.draft.as_ref()
    }

    /// Draws the window.
    pub fn show(&mut self, ctx: &egui::Context, env: &TriggerEnv) -> Vec<TriggerAction> {
        let mut actions = Vec::new();
        let read_only = env.driver == DriverId::Sqlite;
        if let Some(d) = self.draft.as_mut() {
            d.sync(env.triggers);
        }
        widgets::modal(ctx, "Triggers", |ui| {
            ui.set_min_width(640.0);
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(200.0);
                    egui::ScrollArea::vertical().id_salt("trigger_list").max_height(320.0).show(ui, |ui| {
                        for (i, t) in env.triggers.iter().enumerate() {
                            if ui.selectable_label(self.selected == Some(i), format!("{}  {} {}", t.name, t.timing, t.event)).clicked() {
                                self.selected = Some(i);
                                self.confirm_delete = false;
                                self.error = None;
                                self.draft = Some(TriggerDraft::from_info(t, env.driver));
                            }
                        }
                        if env.triggers.is_empty() {
                            muted(ui, "No triggers");
                        }
                    });
                    if !read_only && widgets::secondary_button(ui, "+ Trigger").clicked() {
                        self.start_new(env.driver);
                    }
                });
                ui.separator();
                ui.vertical(|ui| self.form(ui, env, read_only, &mut actions));
            });
            ui.add_space(8.0);
            if widgets::secondary_button(ui, "Close").clicked() {
                actions.push(TriggerAction::Close);
            }
        });
        actions
    }

    /// The right half: the form, or the statement when the trigger cannot be edited.
    fn form(&mut self, ui: &mut egui::Ui, env: &TriggerEnv, read_only: bool, actions: &mut Vec<TriggerAction>) {
        ui.set_min_width(400.0);
        let Some(d) = self.draft.as_mut() else {
            muted(ui, if read_only { "Select a trigger to read it." } else { "Select a trigger or add a new one." });
            return;
        };
        if read_only || !d.editable {
            if let Some(o) = &d.original {
                ui.label(widgets::semibold(format!("{}  {} {}", o.name, o.timing, o.event), 13.0).color(theme::SECONDARY));
                ui.add(egui::Label::new(RichText::new(&o.statement).monospace().color(theme::TEXT)).selectable(true));
            }
            if !read_only {
                muted(ui, NOT_EDITABLE);
            }
        } else {
            widgets::field_label(ui, "Name");
            widgets::input(ui, &mut d.name, "trigger name");
            ui.horizontal(|ui| {
                egui::ComboBox::from_id_salt("trg_timing").selected_text(d.timing.clone()).show_ui(ui, |ui| {
                    for t in TIMINGS {
                        ui.selectable_value(&mut d.timing, t.to_owned(), t);
                    }
                });
                egui::ComboBox::from_id_salt("trg_event").selected_text(d.event.clone()).show_ui(ui, |ui| {
                    for e in EVENTS {
                        ui.selectable_value(&mut d.event, e.to_owned(), e);
                    }
                });
            });
            if d.driver == DriverId::Postgres {
                let level_text = if d.level == TriggerLevel::Row { "FOR EACH ROW" } else { "FOR EACH STATEMENT" };
                egui::ComboBox::from_id_salt("trg_level").selected_text(level_text).show_ui(ui, |ui| {
                    ui.selectable_value(&mut d.level, TriggerLevel::Row, "FOR EACH ROW");
                    ui.selectable_value(&mut d.level, TriggerLevel::Statement, "FOR EACH STATEMENT");
                });
                widgets::field_label(ui, "Function");
                let shown = d.function.as_ref().map(function_label).unwrap_or_else(|| "choose a function".into());
                egui::ComboBox::from_id_salt("trg_function").selected_text(shown).width(300.0).show_ui(ui, |ui| {
                    for f in env.functions {
                        ui.selectable_value(&mut d.function, Some(f.clone()), function_label(f));
                    }
                });
                if env.functions.is_empty() {
                    muted(ui, "This database has no function that returns trigger. Create one in a SQL tab first.");
                }
            } else {
                widgets::field_label(ui, "Body");
                ui.add(egui::TextEdit::multiline(&mut d.body).code_editor().desired_width(f32::INFINITY).desired_rows(10).hint_text("SET NEW.updated = NOW()"));
            }
        }
        if let Some(e) = &self.error {
            ui.label(RichText::new(e).color(theme::ERROR));
        }
        if read_only {
            return;
        }
        ui.add_space(6.0);
        let mut deleted = false;
        ui.horizontal(|ui| {
            if d.editable && widgets::primary_button(ui, "Save").clicked() {
                match d.to_ops(&self.table) {
                    Ok(ops) => {
                        self.error = None;
                        d.submitted = Some(d.name.trim().to_owned());
                        actions.push(TriggerAction::Submit { ops, message: format!("Trigger {} saved", d.name.trim()) });
                    }
                    Err(e) => self.error = Some(e),
                }
            }
            let Some(name) = d.original.as_ref().map(|o| o.name.clone()) else { return };
            if self.confirm_delete {
                muted(ui, &format!("Delete trigger {name}?"));
                if widgets::danger_button(ui, "Delete").clicked() {
                    self.confirm_delete = false;
                    self.selected = None;
                    deleted = true;
                    actions.push(TriggerAction::Submit { ops: delete_ops(&self.table, &name), message: format!("Trigger {name} deleted") });
                }
                if widgets::secondary_button(ui, "Keep").clicked() {
                    self.confirm_delete = false;
                }
            } else if widgets::secondary_button(ui, "Delete").clicked() {
                self.confirm_delete = true;
            }
        });
        if deleted {
            self.draft = None;
        }
    }
}

/// `schema.name` of a trigger function.
fn function_label(f: &TableRef) -> String {
    match &f.schema {
        Some(s) => format!("{s}.{}", f.name),
        None => f.name.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dbzilla_core::ddl::{StructuralOp, TriggerLevel};
    use dbzilla_core::dialect::DriverId;
    use dbzilla_core::model::{TableRef, TriggerInfo};

    fn mysql_info(name: &str) -> TriggerInfo {
        TriggerInfo { name: name.into(), timing: "AFTER".into(), event: "UPDATE".into(), statement: "SET NEW.a = 1".into(), level: "ROW".into(), function: None, editable: true }
    }

    fn filled(driver: DriverId) -> TriggerDraft {
        let mut d = TriggerDraft::blank(driver);
        d.name = "t1".into();
        d.body = "SET NEW.a = 1".into();
        d.function = Some(TableRef { database: None, schema: Some("shop".into()), name: "trg".into() });
        d
    }

    #[test]
    fn new_draft_makes_one_create_op() {
        let ops = filled(DriverId::MySql).to_ops(&TableRef::simple("orders")).unwrap();
        assert!(matches!(ops.as_slice(), [StructuralOp::CreateTrigger { trigger, .. }] if trigger.name == "t1" && trigger.level == TriggerLevel::Row));
    }

    #[test]
    fn edited_draft_drops_the_original_name_then_creates() {
        let mut d = TriggerDraft::from_info(&mysql_info("a"), DriverId::MySql);
        d.name = "b".into();
        let ops = d.to_ops(&TableRef::simple("orders")).unwrap();
        assert!(matches!(ops.as_slice(), [StructuralOp::DropTrigger { name, if_exists: true, .. }, StructuralOp::CreateTrigger { trigger, .. }] if name == "a" && trigger.name == "b"));
    }

    #[test]
    fn delete_makes_one_drop_op() {
        let ops = delete_ops(&TableRef::simple("orders"), "a");
        assert!(matches!(ops.as_slice(), [StructuralOp::DropTrigger { name, if_exists: false, .. }] if name == "a"));
    }

    #[test]
    fn to_ops_reports_the_core_validation_message() {
        let mut d = filled(DriverId::MySql);
        d.name.clear();
        assert!(d.to_ops(&TableRef::simple("orders")).unwrap_err().contains("invalid"));
    }

    #[test]
    fn a_trigger_the_editor_cannot_keep_is_refused() {
        let mut info = mysql_info("a");
        info.editable = false;
        let err = TriggerDraft::from_info(&info, DriverId::Postgres).to_ops(&TableRef::simple("orders")).unwrap_err();
        assert!(err.contains("cannot keep"), "{err}");
    }

    #[test]
    fn postgres_draft_keeps_level_and_function_of_the_original() {
        let mut info = mysql_info("a");
        info.level = "STATEMENT".into();
        info.function = Some(TableRef { database: None, schema: Some("shop".into()), name: "trg".into() });
        let d = TriggerDraft::from_info(&info, DriverId::Postgres);
        assert_eq!(d.level, TriggerLevel::Statement);
        assert_eq!(d.function, info.function);
    }

    /// After a save the list is read again: the draft follows the trigger it became.
    #[test]
    fn sync_follows_a_rename_and_a_first_save() {
        let mut renamed = TriggerDraft::from_info(&mysql_info("a"), DriverId::MySql);
        renamed.name = "b".into();
        renamed.submitted = Some("b".into());
        renamed.sync(&[mysql_info("b")]);
        assert_eq!(renamed.original.as_ref().map(|o| o.name.as_str()), Some("b"), "a later save drops b, not a");
        let mut fresh = filled(DriverId::MySql);
        fresh.submitted = Some("t1".into());
        fresh.sync(&[mysql_info("t1")]);
        assert_eq!(fresh.original.as_ref().map(|o| o.name.as_str()), Some("t1"), "saving a new trigger twice replaces it instead of failing");
    }

    /// A failed MySQL edit leaves no trigger: the draft becomes a new one, so fixing the body and saving works.
    #[test]
    fn sync_turns_a_lost_trigger_into_a_new_one() {
        let mut d = TriggerDraft::from_info(&mysql_info("a"), DriverId::MySql);
        d.body = "SET NEW.a = 2".into();
        d.submitted = Some("a".into());
        d.sync(&[]);
        assert!(d.original.is_none());
        assert_eq!(d.body, "SET NEW.a = 2", "the text typed is kept");
        let ops = d.to_ops(&TableRef::simple("orders")).unwrap();
        assert!(matches!(ops.as_slice(), [StructuralOp::CreateTrigger { .. }]));
    }

    /// A new draft whose name matches an existing trigger, without having been saved, is not that trigger.
    #[test]
    fn sync_never_adopts_a_trigger_that_was_not_saved_as_this_name() {
        let mut d = filled(DriverId::MySql);
        d.sync(&[mysql_info("t1")]);
        assert!(d.original.is_none());
    }

    /// While the list still has the original, the draft is left alone (the reload has not arrived yet).
    #[test]
    fn sync_keeps_a_draft_whose_original_is_still_listed() {
        let mut d = TriggerDraft::from_info(&mysql_info("a"), DriverId::MySql);
        d.name = "b".into();
        d.sync(&[mysql_info("a")]);
        assert_eq!(d.original.as_ref().map(|o| o.name.as_str()), Some("a"));
    }
}
