//! Process list (SPEC 11.4): a sheet over the workspace that lists the running sessions of the server
//! and stops their queries. It works on its own connection, so it answers while the workspace is busy.

use super::jobs::JobEnv;
use super::widgets;
use crate::theme;
use dbzilla_core::dialect::DriverId;
use dbzilla_core::model::{ColumnMeta, Row};
use dbzilla_core::processes;
use dbzilla_drivers::{driver_for, ConnectConfig, Conn, QueryOpts};
use eframe::egui::{self, Align2, Sense, Ui, Vec2};
use egui_extras::{Column, TableBuilder};
use std::collections::BTreeSet;
use std::sync::mpsc as std_mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

/// Seconds between two automatic refreshes.
const AUTO_SECS: u64 = 5;
const ROW_H: f32 = 20.0;

/// Work for the background connection.
enum Cmd {
    List(&'static str),
    Kill(Vec<String>),
}

/// Answers of the background connection.
enum Msg {
    Rows { columns: Vec<ColumnMeta>, rows: Vec<Row> },
    /// Statements run and the errors of those that failed.
    Killed { done: usize, errors: Vec<String> },
    Error(String),
}

/// Requests of the sheet toward the workspace.
#[derive(Debug, Clone, PartialEq)]
pub enum ProcessAction {
    /// Stop the queries of these sessions (ids); the workspace asks the safe mode first.
    Kill(Vec<String>),
    /// Close the sheet.
    Dismiss,
}

/// State of the sheet and handle of its connection. Dropping it closes the connection.
pub struct ProcessList {
    driver: DriverId,
    tx: mpsc::UnboundedSender<Cmd>,
    rx: std_mpsc::Receiver<Msg>,
    columns: Vec<ColumnMeta>,
    rows: Vec<Row>,
    error: Option<String>,
    loading: bool,
    /// Column and direction of the order.
    sort: Option<(usize, bool)>,
    /// Selected session ids.
    selected: BTreeSet<String>,
    /// Row (position on screen) a Shift click extends from.
    anchor: Option<usize>,
    filter: String,
    auto: bool,
    last_refresh: Instant,
    /// What the last kill did, shown under the table.
    notice: Option<(bool, String)>,
}

impl ProcessList {
    /// Opens the sheet and its connection, and asks for the first list.
    pub fn open(env: &JobEnv, repaint: Arc<dyn Fn() + Send + Sync>) -> Self {
        let driver = env.cfg.driver;
        let (tx, mut cmd_rx) = mpsc::unbounded_channel::<Cmd>();
        let (msg_tx, rx) = std_mpsc::channel::<Msg>();
        let cfg = env.cfg.clone();
        env.rt.spawn(async move {
            let mut conn: Option<Box<dyn Conn>> = None;
            while let Some(cmd) = cmd_rx.recv().await {
                let msg = work(&mut conn, &cfg, cmd).await;
                if msg_tx.send(msg).is_err() {
                    break;
                }
                repaint();
            }
        });
        let mut me = Self { driver, tx, rx, columns: Vec::new(), rows: Vec::new(), error: None, loading: false, sort: None, selected: BTreeSet::new(), anchor: None, filter: String::new(), auto: true, last_refresh: Instant::now(), notice: None };
        me.refresh();
        me
    }

    /// Asks for a new list unless one is already on its way.
    pub fn refresh(&mut self) {
        let Some(sql) = processes::list_sql(self.driver) else { return };
        if self.loading {
            return;
        }
        self.loading = true;
        self.last_refresh = Instant::now();
        let _ = self.tx.send(Cmd::List(sql));
    }

    /// Stops the queries with these statements, then reloads the list.
    pub fn kill(&mut self, statements: Vec<String>) {
        let _ = self.tx.send(Cmd::Kill(statements));
    }

    /// Takes in what the connection answered.
    pub fn poll(&mut self) {
        while let Ok(m) = self.rx.try_recv() {
            match m {
                Msg::Rows { columns, rows } => {
                    self.loading = false;
                    self.error = None;
                    self.columns = columns;
                    self.rows = rows;
                    let ids: BTreeSet<String> = self.rows.iter().map(processes::row_id).collect();
                    self.selected.retain(|s| ids.contains(s));
                }
                Msg::Error(e) => {
                    self.loading = false;
                    self.error = Some(e);
                }
                Msg::Killed { done, errors } => {
                    self.notice = Some(if errors.is_empty() { (false, format!("{done} statement(s) sent")) } else { (true, errors.join("; ")) });
                    self.refresh();
                }
            }
        }
    }

    /// Indexes of the rows to show: filtered and sorted.
    fn visible(&self) -> Vec<usize> {
        let order = match self.sort {
            Some((c, asc)) => processes::sorted(&self.rows, c, asc),
            None => (0..self.rows.len()).collect(),
        };
        order.into_iter().filter(|i| processes::matches(&self.rows[*i], &self.filter)).collect()
    }

    /// Ids to act on from a click on row `pos`: the selection when the row is part of it, else the row.
    fn target_ids(&self, id: &str) -> Vec<String> {
        if self.selected.contains(id) { self.selected.iter().cloned().collect() } else { vec![id.to_owned()] }
    }

    /// Applies a click on the row at `pos` of the visible list, following Mod and Shift.
    fn click(&mut self, visible: &[usize], pos: usize, ctrl: bool, shift: bool) {
        let id = processes::row_id(&self.rows[visible[pos]]);
        if shift {
            let from = self.anchor.unwrap_or(pos).min(visible.len() - 1);
            let (a, b) = (from.min(pos), from.max(pos));
            if !ctrl {
                self.selected.clear();
            }
            self.selected.extend(visible[a..=b].iter().map(|i| processes::row_id(&self.rows[*i])));
            return;
        }
        self.anchor = Some(pos);
        if ctrl {
            if !self.selected.remove(&id) {
                self.selected.insert(id);
            }
        } else {
            self.selected.clear();
            self.selected.insert(id);
        }
    }

    /// The rows with these ids, in the order shown.
    fn rows_of(&self, ids: &[String]) -> Vec<Row> {
        self.visible().into_iter().map(|i| &self.rows[i]).filter(|r| ids.contains(&processes::row_id(r))).cloned().collect()
    }
}

/// Rows as tab separated text, or as CSV with a header line when `csv` is set.
fn copy_text(columns: &[ColumnMeta], rows: &[Row], csv: bool) -> String {
    let cell = |text: String| {
        if csv && text.contains(['"', ',', '\n', '\r']) { format!("\"{}\"", text.replace('"', "\"\"")) } else if csv { text } else { text.replace(['\t', '\n'], " ") }
    };
    let sep = if csv { "," } else { "\t" };
    let mut lines = Vec::new();
    if csv {
        lines.push(columns.iter().map(|c| cell(c.name.clone())).collect::<Vec<_>>().join(sep));
    }
    lines.extend(rows.iter().map(|r| r.iter().map(|v| cell(v.to_display())).collect::<Vec<_>>().join(sep)));
    lines.join("\n")
}

/// Runs one command on the background connection, opening it when needed.
async fn work(conn: &mut Option<Box<dyn Conn>>, cfg: &ConnectConfig, cmd: Cmd) -> Msg {
    if conn.is_none() {
        let opened = match driver_for(cfg.driver) {
            Ok(d) => d.connect(cfg).await,
            Err(e) => Err(e),
        };
        match opened {
            Ok(c) => *conn = Some(c),
            Err(e) => return Msg::Error(e.to_string()),
        }
    }
    let Some(c) = conn.as_mut() else { return Msg::Error("no connection".into()) };
    match cmd {
        Cmd::List(sql) => match run(c.as_mut(), sql).await {
            Ok((columns, rows)) => Msg::Rows { columns, rows },
            Err(e) => {
                *conn = None;
                Msg::Error(e)
            }
        },
        Cmd::Kill(statements) => {
            let mut errors = Vec::new();
            let mut done = 0;
            for s in &statements {
                match run(c.as_mut(), s).await {
                    Ok(_) => done += 1,
                    Err(e) => errors.push(format!("{s} {e}")),
                }
            }
            Msg::Killed { done, errors }
        }
    }
}

/// Runs a statement and collects its rows.
pub(super) async fn run(conn: &mut dyn Conn, sql: &str) -> Result<(Vec<ColumnMeta>, Vec<Row>), String> {
    let (tx, mut rx) = mpsc::channel::<dbzilla_drivers::RowBatch>(8);
    let collect = async {
        let (mut columns, mut rows) = (Vec::new(), Vec::new());
        while let Some(b) = rx.recv().await {
            if let Some(c) = b.columns {
                columns = c;
            }
            rows.extend(b.rows);
        }
        (columns, rows)
    };
    let (res, got) = tokio::join!(conn.query_stream(sql, QueryOpts::default(), tx), collect);
    res.map_err(|e| e.to_string())?;
    Ok(got)
}

/// Draws the sheet and returns what it asks of the workspace.
pub fn show(ctx: &egui::Context, list: &mut ProcessList) -> Option<ProcessAction> {
    list.poll();
    if list.auto && !list.loading && list.last_refresh.elapsed() >= Duration::from_secs(AUTO_SECS) {
        list.refresh();
    }
    ctx.request_repaint_after(Duration::from_secs(1));
    let mut action = None;
    let mut open = true;
    egui::Window::new("Process list").id(egui::Id::new("process-list")).open(&mut open).default_size(Vec2::new(980.0, 480.0)).anchor(Align2::CENTER_CENTER, Vec2::ZERO).show(ctx, |ui| {
        toolbar(ui, list, &mut action);
        if let Some(e) = list.error.clone() {
            ui.label(egui::RichText::new(e).color(theme::ERROR));
        }
        table(ui, list, &mut action);
        if let Some((error, text)) = &list.notice {
            ui.label(egui::RichText::new(text).color(if *error { theme::ERROR } else { theme::OK }));
        }
    });
    if !open {
        action = Some(ProcessAction::Dismiss);
    }
    action
}

/// Refresh, kill, filter and dismiss.
fn toolbar(ui: &mut Ui, list: &mut ProcessList, action: &mut Option<ProcessAction>) {
    ui.horizontal(|ui| {
        if widgets::secondary_button(ui, "Refresh").clicked() {
            list.refresh();
        }
        ui.checkbox(&mut list.auto, format!("Every {AUTO_SECS} s"));
        let n = list.selected.len();
        let label = if n > 1 { format!("Kill {n} queries") } else { "Kill".to_owned() };
        if ui.add_enabled(n > 0, egui::Button::new(label)).on_hover_text("Cancel the running query of the selected sessions").clicked() {
            *action = Some(ProcessAction::Kill(list.selected.iter().cloned().collect()));
        }
        widgets::search_box(ui, &mut list.filter, "Filter");
        if list.loading {
            ui.spinner();
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if widgets::secondary_button(ui, "Dismiss").clicked() {
                *action = Some(ProcessAction::Dismiss);
            }
        });
    });
}

/// The sortable, selectable table of sessions.
fn table(ui: &mut Ui, list: &mut ProcessList, action: &mut Option<ProcessAction>) {
    if list.columns.is_empty() {
        let text = if list.loading { "Loading..." } else { "No sessions" };
        ui.label(egui::RichText::new(text).color(theme::MUTED));
        return;
    }
    let visible = list.visible();
    let n = list.columns.len();
    let (ctrl, shift) = ui.input(|i| (i.modifiers.command, i.modifiers.shift));
    let mut clicked = None;
    let mut sort_click = None;
    let mut copy = None;
    let mut kill = None;
    let mut builder = TableBuilder::new(ui).striped(true).sense(Sense::click()).resizable(true).min_scrolled_height(0.0);
    for i in 0..n {
        builder = builder.column(if i + 1 == n { Column::remainder().at_least(160.0).clip(true) } else { Column::auto().at_least(60.0).clip(true) });
    }
    builder
        .header(ROW_H + 4.0, |mut header| {
            for (i, c) in list.columns.iter().enumerate() {
                header.col(|ui| {
                    let arrow = match list.sort {
                        Some((s, true)) if s == i => " ^",
                        Some((s, false)) if s == i => " v",
                        _ => "",
                    };
                    if ui.add(egui::Button::new(egui::RichText::new(format!("{}{arrow}", c.name)).strong().color(theme::SECONDARY)).frame(false)).clicked() {
                        sort_click = Some(i);
                    }
                });
            }
        })
        .body(|body| {
            body.rows(ROW_H, visible.len(), |mut row| {
                let pos = row.index();
                let r = &list.rows[visible[pos]];
                let id = processes::row_id(r);
                row.set_selected(list.selected.contains(&id));
                for v in r {
                    row.col(|ui| {
                        ui.add(egui::Label::new(v.to_display().replace('\n', " ")).selectable(false).truncate());
                    });
                }
                let resp = row.response();
                if resp.clicked() {
                    clicked = Some(pos);
                }
                resp.context_menu(|ui| {
                    if !list.selected.contains(&id) {
                        clicked = Some(pos);
                    }
                    if ui.button("Copy as text").clicked() {
                        copy = Some((false, list.target_ids(&id)));
                        ui.close_menu();
                    }
                    if ui.button("Copy as CSV").clicked() {
                        copy = Some((true, list.target_ids(&id)));
                        ui.close_menu();
                    }
                    if ui.button("Kill").clicked() {
                        kill = Some(list.target_ids(&id));
                        ui.close_menu();
                    }
                });
            });
        });
    if let Some(c) = sort_click {
        list.sort = match list.sort {
            Some((s, true)) if s == c => Some((c, false)),
            Some((s, false)) if s == c => None,
            _ => Some((c, true)),
        };
    }
    if let Some(pos) = clicked {
        list.click(&visible, pos, ctrl, shift);
    }
    if let Some((fmt, ids)) = copy {
        let rows = list.rows_of(&ids);
        ui.ctx().copy_text(copy_text(&list.columns, &rows, fmt));
    }
    if let Some(ids) = kill {
        *action = Some(ProcessAction::Kill(ids));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dbzilla_core::value::Value;

    /// A list that is not connected to anything, with rows set by hand.
    fn list_with(rows: Vec<Row>) -> ProcessList {
        let (tx, _cmd_rx) = mpsc::unbounded_channel();
        let (_msg_tx, rx) = std_mpsc::channel();
        let columns = vec![ColumnMeta { name: "pid".into(), ..ColumnMeta::default() }, ColumnMeta { name: "query".into(), ..ColumnMeta::default() }];
        ProcessList { driver: DriverId::Postgres, tx, rx, columns, rows, error: None, loading: false, sort: None, selected: BTreeSet::new(), anchor: None, filter: String::new(), auto: false, last_refresh: Instant::now(), notice: None }
    }

    fn r(id: i64, q: &str) -> Row {
        vec![Value::I64(id), Value::Text(q.into())]
    }

    #[test]
    fn copy_as_text_and_as_csv() {
        let l = list_with(vec![r(1, "select \"a\", b")]);
        assert_eq!(copy_text(&l.columns, &l.rows, false), "1\tselect \"a\", b");
        assert_eq!(copy_text(&l.columns, &l.rows, true), "pid,query\n1,\"select \"\"a\"\", b\"");
    }

    #[test]
    fn click_selects_one_ctrl_toggles_and_shift_extends() {
        let mut l = list_with(vec![r(1, "a"), r(2, "b"), r(3, "c"), r(4, "d")]);
        let v = l.visible();
        l.click(&v, 1, false, false);
        assert_eq!(l.selected.iter().collect::<Vec<_>>(), ["2"]);
        l.click(&v, 3, true, false);
        assert_eq!(l.selected.len(), 2);
        l.click(&v, 3, true, false);
        assert_eq!(l.selected.iter().collect::<Vec<_>>(), ["2"]);
        l.click(&v, 1, false, false);
        l.click(&v, 3, false, true);
        assert_eq!(l.selected.iter().collect::<Vec<_>>(), ["2", "3", "4"]);
    }

    #[test]
    fn filter_and_sort_decide_what_is_visible() {
        let mut l = list_with(vec![r(10, "select 1"), r(9, "vacuum"), r(100, "select 2")]);
        l.sort = Some((0, true));
        assert_eq!(l.visible(), vec![1, 0, 2]);
        l.filter = "select".into();
        assert_eq!(l.visible(), vec![0, 2]);
    }

    #[test]
    fn acting_on_a_row_outside_the_selection_uses_only_that_row() {
        let mut l = list_with(vec![r(1, "a"), r(2, "b")]);
        l.selected.insert("1".into());
        assert_eq!(l.target_ids("2"), vec!["2"]);
        assert_eq!(l.target_ids("1"), vec!["1"]);
    }

    #[test]
    fn a_new_list_drops_selected_sessions_that_ended() {
        let (tx, _c) = mpsc::unbounded_channel();
        let (msg_tx, rx) = std_mpsc::channel();
        let mut l = list_with(vec![r(1, "a")]);
        l.tx = tx;
        l.rx = rx;
        l.selected.extend(["1".to_owned(), "2".to_owned()]);
        l.loading = true;
        msg_tx.send(Msg::Rows { columns: l.columns.clone(), rows: vec![r(1, "a")] }).unwrap();
        l.poll();
        assert!(!l.loading);
        assert_eq!(l.selected.iter().collect::<Vec<_>>(), ["1"]);
    }

    /// Starts `sleep_sql` on a second connection, finds it in the list, kills it and sees it end.
    fn exercise_server(cfg: ConnectConfig, sleep_sql: &'static str, marker: &str) {
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
        rt.block_on(async {
            for _ in 0..240 {
                if let Ok(mut c) = driver_for(cfg.driver).unwrap().connect(&cfg).await {
                    if c.ping().await.is_ok() {
                        return;
                    }
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            panic!("the database server did not become ready");
        });
        let victim_cfg = cfg.clone();
        let started = Instant::now();
        let victim = rt.spawn(async move {
            let mut c = driver_for(victim_cfg.driver).unwrap().connect(&victim_cfg).await.unwrap();
            let (tx, mut rx) = mpsc::channel::<dbzilla_drivers::RowBatch>(8);
            let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
            let _ = c.query_stream(sleep_sql, QueryOpts::default(), tx).await;
            let _ = drain.await;
        });
        let env = JobEnv { rt: rt.handle().clone(), cfg: cfg.clone(), names: dbzilla_io::naming::NameContext::now("t", "d", "") };
        let mut list = ProcessList::open(&env, Arc::new(|| {}));
        let wait = |list: &mut ProcessList, what: &str, mut done: Box<dyn FnMut(&ProcessList) -> bool>| {
            let t = Instant::now();
            while t.elapsed() < Duration::from_secs(20) {
                list.poll();
                if done(list) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(100));
                list.refresh();
            }
            panic!("timed out: {what}; error {:?}, {} rows, {:?}", list.error, list.rows.len(), list.rows.iter().map(|r| r.iter().map(|v| v.to_display()).collect::<Vec<_>>()).collect::<Vec<_>>());
        };
        let marker = marker.to_owned();
        let m = marker.clone();
        wait(&mut list, "the sleeping query shows up", Box::new(move |l| l.error.is_none() && l.rows.iter().any(|r| r.last().is_some_and(|q| q.to_display().to_lowercase().contains(&m)))));
        assert!(list.error.is_none(), "{:?}", list.error);
        let id = list.rows.iter().find(|r| r.last().is_some_and(|q| q.to_display().to_lowercase().contains(&marker))).map(processes::row_id).unwrap();
        let sql = processes::kill_sql(list.driver, &id).unwrap();
        list.kill(vec![sql]);
        rt.block_on(async { tokio::time::timeout(Duration::from_secs(20), victim).await.expect("the killed query did not end").unwrap() });
        assert!(started.elapsed() < Duration::from_secs(40), "the query ran to its end instead of being killed");
        wait(&mut list, "the kill is reported", Box::new(|l| l.notice.is_some()));
        assert_eq!(list.notice.as_ref().map(|n| n.0), Some(false), "{:?}", list.notice);
    }

    #[test]
    fn postgres_lists_and_cancels_a_running_query() {
        const LABEL: &str = "dbzilla-test-process-pg";
        let Some(c) = crate::testsupport::start(LABEL, "postgres:16-alpine", &[("POSTGRES_PASSWORD", "pw")], 5432) else {
            return eprintln!("SKIPPED: docker is not available");
        };
        let cfg = ConnectConfig { driver: DriverId::Postgres, host: "127.0.0.1".into(), port: Some(c.port), user: "postgres".into(), password: Some("pw".into()), database: "postgres".into(), ssl: dbzilla_core::model::SslMode::Disabled, bootstrap: vec![] };
        let result = std::panic::catch_unwind(|| exercise_server(cfg, "SELECT pg_sleep(60) AS marker_sleep", "marker_sleep"));
        crate::testsupport::cleanup(LABEL);
        if let Err(e) = result {
            std::panic::resume_unwind(e);
        }
    }

    #[test]
    fn mysql_lists_and_kills_a_running_query() {
        const LABEL: &str = "dbzilla-test-process-mysql";
        let Some(c) = crate::testsupport::start(LABEL, "mysql:8.4", &[("MYSQL_ROOT_PASSWORD", "pw"), ("MYSQL_DATABASE", "shop")], 3306) else {
            return eprintln!("SKIPPED: docker is not available");
        };
        let cfg = ConnectConfig { driver: DriverId::MySql, host: "127.0.0.1".into(), port: Some(c.port), user: "root".into(), password: Some("pw".into()), database: "shop".into(), ssl: dbzilla_core::model::SslMode::Disabled, bootstrap: vec![] };
        let result = std::panic::catch_unwind(|| exercise_server(cfg, "SELECT SLEEP(60) AS marker_sleep", "marker_sleep"));
        crate::testsupport::cleanup(LABEL);
        if let Err(e) = result {
            std::panic::resume_unwind(e);
        }
    }

    /// SQL Server is slow to start and big, so this one runs only with `DBZILLA_TEST_MSSQL=1`.
    #[test]
    fn sqlserver_lists_and_kills_a_running_query() {
        const LABEL: &str = "dbzilla-test-process-mssql";
        if std::env::var("DBZILLA_TEST_MSSQL").map(|v| v != "1").unwrap_or(true) {
            return eprintln!("SKIPPED: set DBZILLA_TEST_MSSQL=1 to run the SQL Server tests");
        }
        let Some(c) = crate::testsupport::start(LABEL, "mcr.microsoft.com/mssql/server:2022-latest", &[("ACCEPT_EULA", "Y"), ("MSSQL_SA_PASSWORD", "Dbzilla_Pw1!")], 1433) else {
            return eprintln!("SKIPPED: docker is not available");
        };
        let cfg = ConnectConfig { driver: DriverId::SqlServer, host: "127.0.0.1".into(), port: Some(c.port), user: "sa".into(), password: Some("Dbzilla_Pw1!".into()), database: String::new(), ssl: dbzilla_core::model::SslMode::Required, bootstrap: vec![] };
        let result = std::panic::catch_unwind(|| exercise_server(cfg, "WAITFOR DELAY '00:01:00' -- marker_sleep", "marker_sleep"));
        crate::testsupport::cleanup(LABEL);
        if let Err(e) = result {
            std::panic::resume_unwind(e);
        }
    }

    /// The sheet draws with rows, a selection and an error, and asks to kill the selection.
    #[test]
    fn the_sheet_renders() {
        let mut l = list_with(vec![r(1, "select 1"), r(2, "vacuum")]);
        l.selected.insert("2".into());
        l.error = Some("boom".into());
        l.notice = Some((false, "1 statement(s) sent".into()));
        let ctx = egui::Context::default();
        theme::apply(&ctx);
        for _ in 0..3 {
            let raw = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1200.0, 800.0))), ..Default::default() };
            let _ = ctx.run(raw, |ctx| {
                assert_eq!(show(ctx, &mut l), None);
            });
        }
    }
}
