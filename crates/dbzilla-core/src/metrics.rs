//! Metrics board model (SPEC 11.2): objects driven by SQL, `$name` variables, refresh rules and the
//! shaping of query results into chart data. Pure logic, no UI and no I/O.

use crate::model::{ColumnMeta, Row};
use crate::value::Value;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Shortest and longest automatic refresh interval, in seconds.
pub const MIN_INTERVAL_SECS: u32 = 1;
pub const MAX_INTERVAL_SECS: u32 = 30 * 60;
/// Variable name of an input field or dropdown when the user does not choose one.
pub const DEFAULT_VARIABLE: &str = "input";

/// What an object of the board is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ObjectKind {
    DataTable,
    Scoreboard,
    BarChart,
    LineChart,
    PieChart,
    Input,
    Dropdown,
    Button,
}

impl ObjectKind {
    /// Every kind, in the order of the Add menu.
    pub const ALL: [ObjectKind; 8] = [Self::DataTable, Self::Scoreboard, Self::BarChart, Self::LineChart, Self::PieChart, Self::Input, Self::Dropdown, Self::Button];

    /// Name shown in menus.
    pub fn label(self) -> &'static str {
        match self {
            Self::DataTable => "Data table",
            Self::Scoreboard => "Scoreboard",
            Self::BarChart => "Bar chart",
            Self::LineChart => "Line chart",
            Self::PieChart => "Pie chart",
            Self::Input => "Input field",
            Self::Dropdown => "Dropdown",
            Self::Button => "Button",
        }
    }

    /// True for the objects that run a SQL query to show data.
    pub fn runs_sql(self) -> bool {
        matches!(self, Self::DataTable | Self::Scoreboard | Self::BarChart | Self::LineChart | Self::PieChart)
    }

    /// True for the objects that hold a value other objects read as `$name`.
    pub fn is_variable(self) -> bool {
        matches!(self, Self::Input | Self::Dropdown)
    }
}

/// When an object reloads its data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Refresh {
    /// Every `n` seconds, between 1 s and 30 min.
    Every(u32),
    /// Only when a connected object emits an event, or its variable changes.
    OnEvent,
    /// Only with the refresh button of the object.
    Manual,
}

impl Refresh {
    /// The same rule with the interval kept in range.
    pub fn clamped(self) -> Self {
        match self {
            Self::Every(s) => Self::Every(s.clamp(MIN_INTERVAL_SECS, MAX_INTERVAL_SECS)),
            other => other,
        }
    }

    /// True when an automatic refresh is due: `last_ms` is when the data was last requested.
    pub fn is_due(self, now_ms: u64, last_ms: Option<u64>) -> bool {
        match (self.clamped(), last_ms) {
            (Self::Every(_), None) => true,
            (Self::Every(s), Some(last)) => now_ms.saturating_sub(last) >= u64::from(s) * 1000,
            _ => false,
        }
    }
}

/// One object on the canvas.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MetricObject {
    pub id: u64,
    pub kind: ObjectKind,
    pub label: String,
    pub sql: String,
    pub refresh: Refresh,
    /// Position and size on the canvas.
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    /// Variable name of an input field or dropdown, and initial value.
    pub name: String,
    pub value: String,
    /// Fixed choices of a dropdown, used when `sql` is empty.
    pub choices: Vec<String>,
    /// Objects that refresh when this one emits an event (button click, or a variable submitted).
    pub targets: Vec<u64>,
}

impl Default for MetricObject {
    fn default() -> Self {
        Self { id: 0, kind: ObjectKind::DataTable, label: String::new(), sql: String::new(), refresh: Refresh::Manual, x: 0.0, y: 0.0, w: 320.0, h: 220.0, name: String::new(), value: String::new(), choices: Vec::new(), targets: Vec::new() }
    }
}

impl MetricObject {
    /// A new object of `kind` with sensible defaults at (`x`, `y`).
    pub fn new(id: u64, kind: ObjectKind, x: f32, y: f32) -> Self {
        let (w, h) = match kind {
            ObjectKind::Scoreboard => (260.0, 160.0),
            ObjectKind::Input | ObjectKind::Dropdown => (220.0, 70.0),
            ObjectKind::Button => (160.0, 60.0),
            _ => (360.0, 240.0),
        };
        let name = if kind.is_variable() { DEFAULT_VARIABLE.to_owned() } else { String::new() };
        let refresh = if kind.runs_sql() { Refresh::Every(30) } else { Refresh::Manual };
        let label = if kind == ObjectKind::Button { "Run".to_owned() } else { kind.label().to_owned() };
        Self { id, kind, label, refresh, x, y, w, h, name, ..Self::default() }
    }
}

/// A named canvas of objects.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Board {
    pub id: String,
    pub name: String,
    pub objects: Vec<MetricObject>,
}

impl Default for Board {
    fn default() -> Self {
        Self { id: String::new(), name: "Metrics".into(), objects: Vec::new() }
    }
}

/// Errors from reading a board file.
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum BoardError {
    #[error("not a metrics board: {0}")]
    Invalid(String),
    #[error("the board uses object id {0} twice")]
    DuplicateId(u64),
}

impl Board {
    /// A new empty board.
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self { id: id.into(), name: name.into(), objects: Vec::new() }
    }

    /// Next free object id.
    pub fn next_id(&self) -> u64 {
        self.objects.iter().map(|o| o.id).max().unwrap_or(0) + 1
    }

    /// Adds an object of `kind` and returns its id.
    pub fn add(&mut self, kind: ObjectKind, x: f32, y: f32) -> u64 {
        let id = self.next_id();
        self.objects.push(MetricObject::new(id, kind, x, y));
        id
    }

    /// Removes an object and every connection that pointed at it.
    pub fn remove(&mut self, id: u64) {
        self.objects.retain(|o| o.id != id);
        for o in &mut self.objects {
            o.targets.retain(|t| *t != id);
        }
    }

    /// The object with `id`.
    pub fn get(&self, id: u64) -> Option<&MetricObject> {
        self.objects.iter().find(|o| o.id == id)
    }

    /// Mutable access to the object with `id`.
    pub fn get_mut(&mut self, id: u64) -> Option<&mut MetricObject> {
        self.objects.iter_mut().find(|o| o.id == id)
    }

    /// Current value of every variable object, by name.
    pub fn variables(&self) -> BTreeMap<String, String> {
        self.objects.iter().filter(|o| o.kind.is_variable()).map(|o| (variable_name(o), o.value.clone())).collect()
    }

    /// Objects to refresh when `source` emits an event or changes its variable: its targets, and the
    /// objects whose SQL reads the variable.
    pub fn affected_by(&self, source: u64) -> Vec<u64> {
        let Some(src) = self.get(source) else { return Vec::new() };
        let mut out: Vec<u64> = src.targets.iter().copied().filter(|t| self.get(*t).is_some()).collect();
        if src.kind.is_variable() {
            let name = variable_name(src);
            for o in self.objects.iter().filter(|o| o.kind.runs_sql() && variables_used(&o.sql).contains(&name)) {
                if !out.contains(&o.id) {
                    out.push(o.id);
                }
            }
        }
        out
    }

    /// The board as pretty JSON, for Export Metrics Board.
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }

    /// Reads a board exported with [`Board::to_json`]; the intervals are kept in range.
    pub fn from_json(text: &str) -> Result<Self, BoardError> {
        let mut board: Board = serde_json::from_str(text).map_err(|e| BoardError::Invalid(e.to_string()))?;
        let mut seen = Vec::new();
        for o in &mut board.objects {
            if seen.contains(&o.id) {
                return Err(BoardError::DuplicateId(o.id));
            }
            seen.push(o.id);
            o.refresh = o.refresh.clamped();
        }
        Ok(board)
    }
}

/// Variable name of an object: its name, or the default one.
pub fn variable_name(o: &MetricObject) -> String {
    let n = o.name.trim();
    if n.is_empty() { DEFAULT_VARIABLE.to_owned() } else { n.to_owned() }
}

/// True for the characters of a variable name.
fn is_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// Names of the `$name` variables written in `sql`, without repeats.
pub fn variables_used(sql: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let chars: Vec<char> = sql.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '$' && chars.get(i + 1).is_some_and(|c| is_name_char(*c)) {
            let start = i + 1;
            let mut end = start;
            while end < chars.len() && is_name_char(chars[end]) {
                end += 1;
            }
            let name: String = chars[start..end].iter().collect();
            if !out.contains(&name) {
                out.push(name);
            }
            i = end;
        } else {
            i += 1;
        }
    }
    out
}

/// Replaces `$name` with the value of the variable, exactly as typed (the user quotes strings). Names
/// that are not variables, like the `$1` of a prepared statement, stay as they are.
pub fn substitute(sql: &str, vars: &BTreeMap<String, String>) -> String {
    let chars: Vec<char> = sql.chars().collect();
    let mut out = String::with_capacity(sql.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '$' && chars.get(i + 1).is_some_and(|c| is_name_char(*c)) {
            let start = i + 1;
            let mut end = start;
            while end < chars.len() && is_name_char(chars[end]) {
                end += 1;
            }
            let name: String = chars[start..end].iter().collect();
            match vars.get(&name) {
                Some(v) => out.push_str(v),
                None => out.extend(&chars[i..end]),
            }
            i = end;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

/// A number from a cell, when it holds one.
pub fn value_to_f64(v: &Value) -> Option<f64> {
    match v {
        Value::I64(n) => Some(*n as f64),
        Value::U64(n) => Some(*n as f64),
        Value::F64(n) => Some(*n),
        Value::Bool(b) => Some(f64::from(u8::from(*b))),
        Value::Decimal(s) | Value::Text(s) | Value::Other(s) => s.trim().parse().ok().filter(|n: &f64| n.is_finite()),
        _ => None,
    }
}

/// Label and value pairs for a bar or pie chart: first column is the label, second the value. Rows
/// whose value is not a number are skipped.
pub fn label_values(rows: &[Row]) -> Vec<(String, f64)> {
    rows.iter().filter_map(|r| Some((r.first()?.to_display(), value_to_f64(r.get(1)?)?))).collect()
}

/// One line of a line chart.
#[derive(Debug, Clone, PartialEq)]
pub struct Series {
    pub name: String,
    pub points: Vec<f64>,
}

/// Data of a line chart: x labels from the first column and one series per remaining numeric column.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct LineData {
    pub x: Vec<String>,
    pub series: Vec<Series>,
}

/// Shapes a result for a line chart. A column that holds no number at all is not a series.
pub fn line_data(columns: &[ColumnMeta], rows: &[Row]) -> LineData {
    let x = rows.iter().map(|r| r.first().map(Value::to_display).unwrap_or_default()).collect();
    let series = (1..columns.len())
        .filter(|c| rows.iter().any(|r| r.get(*c).and_then(value_to_f64).is_some()))
        .map(|c| Series { name: columns[c].name.clone(), points: rows.iter().map(|r| r.get(c).and_then(value_to_f64).unwrap_or(f64::NAN)).collect() })
        .collect();
    LineData { x, series }
}

/// One entry of a scoreboard.
#[derive(Debug, Clone, PartialEq)]
pub struct Score {
    pub score: String,
    pub detail: String,
}

/// Scoreboard entries: the Score column is the first, the Detail column the second (when present).
pub fn scores(rows: &[Row]) -> Vec<Score> {
    rows.iter().map(|r| Score { score: r.first().map(Value::to_display).unwrap_or_default(), detail: r.get(1).map(Value::to_display).unwrap_or_default() }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(items: &[(&str, &str)]) -> BTreeMap<String, String> {
        items.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect()
    }

    fn col(name: &str) -> ColumnMeta {
        ColumnMeta { name: name.into(), ..ColumnMeta::default() }
    }

    #[test]
    fn substitutes_known_variables_exactly_as_typed() {
        let v = vars(&[("city", "'Rome'"), ("n", "5")]);
        assert_eq!(substitute("select * from t where c = $city limit $n", &v), "select * from t where c = 'Rome' limit 5");
    }

    #[test]
    fn unknown_names_and_positional_parameters_stay() {
        let v = vars(&[("a", "1")]);
        assert_eq!(substitute("select $1, $b, $a", &v), "select $1, $b, 1");
    }

    #[test]
    fn longest_name_wins() {
        let v = vars(&[("id", "1"), ("id2", "2")]);
        assert_eq!(substitute("$id2 $id", &v), "2 1");
    }

    #[test]
    fn lists_variables_once() {
        assert_eq!(variables_used("select $a, $b, $a, $1"), vec!["a", "b", "1"]);
    }

    #[test]
    fn interval_is_clamped_and_due_by_time() {
        assert_eq!(Refresh::Every(0).clamped(), Refresh::Every(1));
        assert_eq!(Refresh::Every(99_999).clamped(), Refresh::Every(1800));
        assert!(Refresh::Every(5).is_due(0, None));
        assert!(!Refresh::Every(5).is_due(4_999, Some(0)));
        assert!(Refresh::Every(5).is_due(5_000, Some(0)));
        assert!(!Refresh::OnEvent.is_due(1_000_000, None));
        assert!(!Refresh::Manual.is_due(1_000_000, Some(0)));
    }

    #[test]
    fn board_adds_removes_and_cleans_targets() {
        let mut b = Board::new("b1", "Main");
        let btn = b.add(ObjectKind::Button, 0.0, 0.0);
        let tbl = b.add(ObjectKind::DataTable, 10.0, 10.0);
        assert_eq!((btn, tbl), (1, 2));
        b.get_mut(btn).unwrap().targets.push(tbl);
        assert_eq!(b.affected_by(btn), vec![tbl]);
        b.remove(tbl);
        assert!(b.get(btn).unwrap().targets.is_empty());
        assert_eq!(b.add(ObjectKind::Input, 0.0, 0.0), 2);
    }

    #[test]
    fn a_variable_refreshes_the_objects_that_read_it() {
        let mut b = Board::new("b", "B");
        let input = b.add(ObjectKind::Input, 0.0, 0.0);
        let reads = b.add(ObjectKind::Scoreboard, 0.0, 0.0);
        let other = b.add(ObjectKind::Scoreboard, 0.0, 0.0);
        b.get_mut(reads).unwrap().sql = "select 1 where x = $input".into();
        b.get_mut(other).unwrap().sql = "select 1".into();
        b.get_mut(input).unwrap().value = "'x'".into();
        assert_eq!(b.affected_by(input), vec![reads]);
        assert_eq!(b.variables(), vars(&[("input", "'x'")]));
    }

    #[test]
    fn empty_variable_name_falls_back_to_input() {
        let mut o = MetricObject::new(1, ObjectKind::Dropdown, 0.0, 0.0);
        o.name = "  ".into();
        assert_eq!(variable_name(&o), "input");
    }

    #[test]
    fn json_round_trip_and_validation() {
        let mut b = Board::new("b", "Main");
        b.add(ObjectKind::BarChart, 1.0, 2.0);
        b.get_mut(1).unwrap().refresh = Refresh::Every(100_000);
        let back = Board::from_json(&b.to_json()).unwrap();
        assert_eq!(back.objects[0].refresh, Refresh::Every(1800));
        assert!(matches!(Board::from_json("nope"), Err(BoardError::Invalid(_))));
        let mut dup = b.clone();
        dup.objects.push(dup.objects[0].clone());
        assert_eq!(Board::from_json(&dup.to_json()), Err(BoardError::DuplicateId(1)));
    }

    #[test]
    fn label_values_skip_non_numbers() {
        let rows = vec![vec![Value::Text("a".into()), Value::I64(3)], vec![Value::Text("b".into()), Value::Text("x".into())], vec![Value::Text("c".into()), Value::Decimal("2.5".into())]];
        assert_eq!(label_values(&rows), vec![("a".to_owned(), 3.0), ("c".to_owned(), 2.5)]);
    }

    #[test]
    fn line_data_has_one_series_per_numeric_column() {
        let cols = vec![col("day"), col("sales"), col("note"), col("cost")];
        let rows = vec![
            vec![Value::Text("mon".into()), Value::I64(1), Value::Text("a".into()), Value::F64(0.5)],
            vec![Value::Text("tue".into()), Value::I64(4), Value::Null, Value::Null],
        ];
        let d = line_data(&cols, &rows);
        assert_eq!(d.x, vec!["mon", "tue"]);
        assert_eq!(d.series.len(), 2);
        assert_eq!(d.series[0], Series { name: "sales".into(), points: vec![1.0, 4.0] });
        assert_eq!(d.series[1].name, "cost");
        assert!(d.series[1].points[1].is_nan());
    }

    #[test]
    fn scores_take_score_then_detail() {
        let rows = vec![vec![Value::I64(42), Value::Text("users".into())], vec![Value::I64(7)]];
        assert_eq!(scores(&rows), vec![Score { score: "42".into(), detail: "users".into() }, Score { score: "7".into(), detail: String::new() }]);
    }
}
