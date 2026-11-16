//! Parsing, preview, mapping and type detection (SPEC 12.1).

use dbzilla_io::import::*;

#[test]
fn csv_options_and_header() {
    let p = parse_csv(b"a;b\n1;'x;y'\n", &CsvImportOptions { delimiter: ';', quote: '\'', ..CsvImportOptions::default() }).unwrap();
    assert_eq!(p.headers, vec!["a", "b"]);
    assert_eq!(p.rows, vec![vec![Some("1".to_owned()), Some("x;y".to_owned())]]);
    let p = parse_csv(b"1,2\n3,4\n", &CsvImportOptions { header: false, ..CsvImportOptions::default() }).unwrap();
    assert_eq!(p.headers, vec!["column1", "column2"]);
    assert_eq!(p.rows.len(), 2);
}

#[test]
fn csv_null_text_and_empty_values() {
    let p = parse_csv(b"a,b\nNULL,\n", &CsvImportOptions::default()).unwrap();
    assert_eq!(p.rows[0], vec![None, Some(String::new())]);
    let p = parse_csv(b"a\nNULL\n", &CsvImportOptions { null_text: None, ..CsvImportOptions::default() }).unwrap();
    assert_eq!(p.rows[0], vec![Some("NULL".to_owned())]);
}

#[test]
fn csv_encodings() {
    let latin1: &[u8] = b"name\ncaf\xe9\n";
    let p = parse_csv(latin1, &CsvImportOptions { encoding: "windows-1252".into(), ..CsvImportOptions::default() }).unwrap();
    assert_eq!(p.rows[0][0].as_deref(), Some("café"));
    assert!(parse_csv(latin1, &CsvImportOptions::default()).is_err(), "invalid utf-8 is reported, not mangled");
    assert!(parse_csv(b"a\n", &CsvImportOptions { encoding: "no-such-encoding".into(), ..CsvImportOptions::default() }).is_err());
    let bom = parse_csv(b"\xef\xbb\xbfid\n1\n", &CsvImportOptions::default()).unwrap();
    assert_eq!(bom.headers, vec!["id"], "a UTF-8 byte order mark is dropped");
}

#[test]
fn ragged_rows_are_padded_or_reported() {
    let p = parse_csv(b"a,b,c\n1,2\n", &CsvImportOptions::default()).unwrap();
    assert_eq!(p.rows[0].len(), 3);
    assert_eq!(p.rows[0][2], None);
}

#[test]
fn json_arrays_of_objects() {
    let p = parse_json(br#"[{"a":1,"b":"x"},{"b":null,"c":true,"a":2.5}]"#).unwrap();
    assert_eq!(p.headers, vec!["a", "b", "c"]);
    assert_eq!(p.rows[0], vec![Some("1".into()), Some("x".into()), None]);
    assert_eq!(p.rows[1], vec![Some("2.5".into()), None, Some("true".into())]);
    assert!(parse_json(b"{\"a\":1}").is_err());
    assert!(parse_json(b"[1,2]").is_err());
    let nested = parse_json(br#"[{"a":{"b":1}}]"#).unwrap();
    assert_eq!(nested.rows[0][0].as_deref(), Some("{\"b\":1}"));
}

#[test]
fn preview_is_limited() {
    let mut csv = String::from("n\n");
    for i in 0..250 {
        csv.push_str(&format!("{i}\n"));
    }
    let p = parse_csv(csv.as_bytes(), &CsvImportOptions::default()).unwrap();
    assert_eq!(p.rows.len(), 250);
    assert_eq!(preview(&p).rows.len(), PREVIEW_ROWS);
}

#[test]
fn column_matching_strategies() {
    let src: Vec<String> = ["Id", "name", "extra"].iter().map(|s| (*s).to_owned()).collect();
    let dst: Vec<String> = ["id", "name", "other"].iter().map(|s| (*s).to_owned()).collect();
    assert_eq!(match_columns(MatchStrategy::Name, &src, &dst), vec![None, Some(1), None]);
    assert_eq!(match_columns(MatchStrategy::NameCaseInsensitive, &src, &dst), vec![Some(0), Some(1), None]);
    assert_eq!(match_columns(MatchStrategy::Order, &src, &dst), vec![Some(0), Some(1), Some(2)]);
    assert_eq!(match_columns(MatchStrategy::NameAndOrder, &src, &dst), vec![Some(0), Some(1), Some(2)], "exact names first, the rest by order");
}

#[test]
fn type_detection() {
    let p = parse_csv(b"i,r,t,e,m\n1,1.5,a,,1\n2,2,b,,x\n-3,0.1,c,,3\n", &CsvImportOptions::default()).unwrap();
    use DetectedType::*;
    assert_eq!(detect_types(&p), vec![Integer, Real, Text, Text, Text]);
}
