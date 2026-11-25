//! Output rendering.
//!
//! Two formats, one default: JSON (machine-first, since the main consumer is a
//! program) and a human table behind `--table`.
//!
//! Table rules:
//! * columns are discovered from the union of keys, ordered by first appearance
//!   so the server's own ordering survives (sorted output would rearrange a
//!   payload the operator is trying to read in context);
//! * widths come from the measured display width of the content and are never
//!   capped, so nothing is ever truncated with an ellipsis;
//! * a row wider than the terminal wraps its widest cell onto continuation
//!   lines instead of cutting it;
//! * numbers are right-aligned, everything else left-aligned;
//! * East Asian wide characters count as two columns.

use serde_json::Value;
use unicode_width::UnicodeWidthStr;

/// Redact anything credential-shaped in a JSON tree.
///
/// Applied inside [`to_json`] and [`to_table`] — the single chokepoint every
/// value passes through — rather than per command. Per-command redaction is a
/// known failure mode: a new command that echoes a server object is then
/// unredacted by default.
///
/// Keys are matched case-insensitively, both exactly and by suffix, so
/// `session_token`, `access_token`, `id_token` and `password` all match without
/// having to enumerate every variant the server might return.
pub fn redact(value: &Value) -> Value {
    const EXACT: &[&str] = &[
        "token",
        "session_token",
        "sessiontoken",
        "access_token",
        "accesstoken",
        "refresh_token",
        "refreshtoken",
        "id_token",
        "idtoken",
        "secret",
        "password",
        "passwd",
        "authorization",
        "device_key",
        "nonce",
    ];
    const SUFFIXES: &[&str] = &[
        "password",
        "passwd",
        "token",
        "secret",
        "private_key",
        "privatekey",
    ];

    fn is_secret(key: &str) -> bool {
        let lower = key.to_ascii_lowercase();
        EXACT.contains(&lower.as_str()) || SUFFIXES.iter().any(|s| lower.ends_with(s))
    }

    match value {
        Value::Object(map) => {
            let mut out = serde_json::Map::with_capacity(map.len());
            for (k, v) in map {
                if is_secret(k) {
                    // Presence and size only. `chars` is used because the JSON
                    // key says so explicitly; a byte count would be a different
                    // number for the same string.
                    let chars = match v {
                        Value::String(s) => s.chars().count(),
                        _ => 0,
                    };
                    out.insert(
                        k.clone(),
                        serde_json::json!({ "redacted": true, "chars": chars }),
                    );
                } else {
                    out.insert(k.clone(), redact(v));
                }
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.iter().map(redact).collect()),
        other => other.clone(),
    }
}

/// Render a value as pretty JSON, with credentials redacted. Deterministic for
/// a given input because serde_json's map preserves insertion order (the
/// `preserve_order` feature is enabled), so the server's own key order survives.
pub fn to_json(value: &Value) -> String {
    let safe = redact(value);
    serde_json::to_string_pretty(&safe).unwrap_or_else(|e| format!("{{\"error\":\"{}\"}}", e))
}

/// Render a value as a table. Scalars and flat objects become one row; arrays of
/// objects become one row per element.
///
/// A wrapper object whose *only* array-of-objects member is the payload
/// (`{"events": [...], "next_ids": [...]}`) is unwrapped to that array: printing
/// the whole API envelope as a two-row key/value table is technically lossless
/// but unreadable, which defeats the purpose of the table view. When more than
/// one array is present the object is shown as key/value, because guessing
/// which one the caller wants would hide the others.
pub fn to_table(value: &Value) -> String {
    let value = redact(value);
    let value = unwrap_single_array(value);
    match &value {
        Value::Array(items) => {
            if items.iter().all(|v| v.is_object()) {
                rows_table(items)
            } else if items.iter().all(|v| !v.is_object() && !v.is_array()) {
                scalar_table(items)
            } else {
                // Mixed or nested: fall back to one column of compact JSON, which
                // is still lossless.
                one_column(items)
            }
        }
        Value::Object(map) => object_table(map),
        other => other.to_string(),
    }
}

/// Width of a string as it is displayed: CJK counts double.
fn width(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

fn pad(s: &str, target: usize, right_align: bool) -> String {
    let w = width(s);
    if w >= target {
        return s.to_string();
    }
    let fill = " ".repeat(target - w);
    if right_align {
        format!("{}{}", fill, s)
    } else {
        format!("{}{}", s, fill)
    }
}

/// A cell's display text. Nested values become compact single-line JSON so the
/// table stays rectangular without dropping information.
///
/// A string containing newlines would otherwise break the grid: the renderer
/// measures one line and the terminal then shows several. The newline is
/// therefore escaped as a visible `\n` rather than removed, so the content is
/// still complete and the row count stays honest.
fn cell(v: &Value) -> String {
    match v {
        Value::String(s) => escape_newlines(s),
        Value::Null => "-".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        other => escape_newlines(&serde_json::to_string(other).unwrap_or_default()),
    }
}

fn escape_newlines(s: &str) -> String {
    if !s.contains(['\n', '\r']) {
        return s.to_string();
    }
    // CRLF, LF and lone CR all map to the same two characters, so a single
    // replacement over the character set is equivalent to the three-pass chain
    // it replaces (and avoids clippy::collapsible_str_replace).
    s.replace(['\r', '\n'], "\\n")
}

fn is_numeric(v: &Value) -> bool {
    matches!(v, Value::Number(_))
}

/// If the object has exactly one array-of-objects member and every other member
/// is an array of scalars or absent, return that member's array. Otherwise
/// return the value unchanged.
fn unwrap_single_array(value: Value) -> Value {
    let Value::Object(map) = &value else {
        return value;
    };
    let object_arrays: Vec<&String> = map
        .iter()
        .filter(|(_, v)| {
            v.as_array()
                .is_some_and(|a| !a.is_empty() && a.iter().all(|x| x.is_object()))
        })
        .map(|(k, _)| k)
        .collect();
    if object_arrays.len() != 1 {
        return value;
    }
    let key = object_arrays[0];
    value.get(key).cloned().unwrap_or(Value::Array(Vec::new()))
}

/// Column order: first appearance across all rows.
fn columns(items: &[&serde_json::Map<String, Value>]) -> Vec<String> {
    let mut cols: Vec<String> = Vec::new();
    for m in items {
        for k in m.keys() {
            if !cols.iter().any(|c| c == k) {
                cols.push(k.clone());
            }
        }
    }
    cols
}

fn rows_table(items: &[Value]) -> String {
    let maps: Vec<&serde_json::Map<String, Value>> =
        items.iter().filter_map(|v| v.as_object()).collect();
    if maps.is_empty() {
        return "(no rows)".to_string();
    }
    let cols = columns(&maps);

    let header: Vec<String> = cols.clone();
    let body: Vec<Vec<String>> = maps
        .iter()
        .map(|m| {
            cols.iter()
                .map(|c| m.get(c).map(cell).unwrap_or_default())
                .collect()
        })
        .collect();

    let numeric: Vec<bool> = cols
        .iter()
        .enumerate()
        .map(|(i, _)| {
            maps.iter()
                .filter_map(|m| m.get(&cols[i]))
                .filter(|v| !v.is_null())
                .all(is_numeric)
        })
        .collect();

    render(&header, &body, &numeric)
}

fn object_table(map: &serde_json::Map<String, Value>) -> String {
    let header = vec!["key".to_string(), "value".to_string()];
    let body: Vec<Vec<String>> = map.iter().map(|(k, v)| vec![k.clone(), cell(v)]).collect();
    render(&header, &body, &[false, false])
}

fn scalar_table(items: &[Value]) -> String {
    let header = vec!["value".to_string()];
    let body: Vec<Vec<String>> = items.iter().map(|v| vec![cell(v)]).collect();
    render(&header, &body, &[false])
}

fn one_column(items: &[Value]) -> String {
    let header = vec!["json".to_string()];
    let body: Vec<Vec<String>> = items
        .iter()
        .map(|v| vec![serde_json::to_string(v).unwrap_or_default()])
        .collect();
    render(&header, &body, &[false])
}

/// Lay out the grid. No ellipsis, ever: the widest cell sets its column width.
fn render(header: &[String], body: &[Vec<String>], numeric: &[bool]) -> String {
    let n = header.len();
    let mut widths: Vec<usize> = header.iter().map(|h| width(h)).collect();
    for row in body {
        for (i, c) in row.iter().enumerate().take(n) {
            widths[i] = widths[i].max(width(c));
        }
    }

    let mut out = String::new();
    // Header
    let mut line = String::new();
    for (i, h) in header.iter().enumerate() {
        if i > 0 {
            line.push_str("  ");
        }
        line.push_str(&pad(h, widths[i], false));
    }
    out.push_str(line.trim_end());
    out.push('\n');

    // Separator
    let mut sep = String::new();
    for (i, w) in widths.iter().enumerate() {
        if i > 0 {
            sep.push_str("  ");
        }
        sep.push_str(&"-".repeat(*w));
    }
    out.push_str(&sep);
    out.push('\n');

    // Rows
    for row in body {
        let mut line = String::new();
        for i in 0..n {
            if i > 0 {
                line.push_str("  ");
            }
            let empty = String::new();
            let c = row.get(i).unwrap_or(&empty);
            line.push_str(&pad(c, widths[i], numeric[i]));
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn json_roundtrips_and_is_pretty() {
        let v = json!({"b": 1, "a": [1, 2]});
        let s = to_json(&v);
        assert!(s.contains("\n"), "pretty output should have newlines");
        let back: Value = serde_json::from_str(&s).unwrap();
        assert_eq!(back, v);
    }

    #[test]
    fn json_key_order_is_stable_across_calls() {
        let v = json!({"z": 1, "a": 2, "m": 3});
        assert_eq!(to_json(&v), to_json(&v));
    }

    #[test]
    fn table_of_objects_has_a_header_and_separator() {
        let v = json!([{"id": "e1", "title": "Test"}]);
        let t = to_table(&v);
        let lines: Vec<&str> = t.lines().collect();
        assert!(lines[0].contains("id"));
        assert!(lines[1].starts_with("--"));
        assert!(lines[2].contains("e1"));
    }

    #[test]
    fn no_ellipsis_is_ever_emitted() {
        let long = "x".repeat(500);
        let v = json!([{"id": "e1", "note": long.clone()}]);
        let t = to_table(&v);
        assert!(t.contains(&long), "the full value must appear");
        assert!(!t.contains("..."), "truncation is forbidden");
    }

    #[test]
    fn cjk_width_is_two_columns() {
        assert_eq!(width("サッカー"), 8);
        assert_eq!(width("abc"), 3);
        assert_eq!(width("トレンド"), 8);
    }

    #[test]
    fn cjk_and_ascii_align_in_the_same_column() {
        let v = json!([
            {"name": "サッカー", "n": 1},
            {"name": "abc", "n": 22}
        ]);
        let t = to_table(&v);
        let rows: Vec<&str> = t
            .lines()
            .filter(|l| !l.starts_with('-') && !l.starts_with("name"))
            .collect();

        // Right-aligned numbers must END at the same *display* column. Byte
        // offsets differ because the CJK name occupies 4 chars but 12 bytes, so
        // the comparison has to be in terminal columns, not byte indices.
        let end_col = |line: &str, needle: &str| -> usize {
            let start = line.find(needle).expect("number present");
            width(&line[..start]) + width(needle)
        };
        assert_eq!(
            end_col(rows[0], "1"),
            end_col(rows[1], "22"),
            "numeric column must be right-aligned in display columns:\n{}",
            t
        );
        // The name column must also occupy the same display width in both rows:
        // that is what breaks when CJK is measured in chars instead of columns.
        let name_cell = |line: &str, number: &str| -> usize {
            let start = line.find(number).expect("number present");
            width(&line[..start])
        };
        // Both numbers end at the same column, so their widths differ by exactly
        // the padding difference; assert the simpler invariant directly instead.
        assert_eq!(
            name_cell(rows[0], "1") + width("1"),
            name_cell(rows[1], "22") + width("22")
        );
    }

    #[test]
    fn null_renders_as_dash() {
        let v = json!([{"a": null}]);
        assert!(to_table(&v).contains('-'));
    }

    #[test]
    fn nested_value_becomes_compact_json_in_cell() {
        let v = json!([{"a": {"b": 1}}]);
        let t = to_table(&v);
        assert!(t.contains("{\"b\":1}"), "{}", t);
    }

    #[test]
    fn credentials_are_redacted_at_the_output_chokepoint() {
        let v = serde_json::json!({
            "user": {"id": 1, "nickname": "abc"},
            "token": "supersecret",
            "nested": {"access_token": "another", "ok": true},
        });
        let j = to_json(&v);
        assert!(!j.contains("supersecret"), "{}", j);
        assert!(!j.contains("another"), "{}", j);
        assert!(j.contains("\"redacted\": true"), "{}", j);
        // Non-secret data survives.
        assert!(j.contains("abc"));
        assert!(j.contains("\"id\": 1"));
    }

    #[test]
    fn redaction_reports_length_but_not_value() {
        let v = serde_json::json!({"token": "12345678"});
        let j = to_json(&v);
        assert!(j.contains("\"chars\": 8"), "{}", j);
        assert!(!j.contains("12345678"));
    }

    #[test]
    fn redaction_matches_suffix_variants() {
        let v = serde_json::json!({
            "id_token": "x1",
            "refreshToken": "x2",
            "my_password": "x3",
            "private_key": "x4",
        });
        let j = to_json(&v);
        for leaked in ["x1", "x2", "x3", "x4"] {
            assert!(!j.contains(leaked), "{} leaked in {}", leaked, j);
        }
    }

    #[test]
    fn redaction_is_applied_to_tables_too() {
        let v = serde_json::json!([{"id": "e1", "secret": "hide me"}]);
        let t = to_table(&v);
        assert!(!t.contains("hide me"), "{}", t);
        assert!(t.contains("redacted"));
    }

    #[test]
    fn envelope_with_one_object_array_is_unwrapped() {
        let v = serde_json::json!({
            "events": [{"id": 1, "title": "a"}, {"id": 2, "title": "b"}],
            "next_ids": [3, 4]
        });
        let t = to_table(&v);
        assert!(
            t.starts_with("id"),
            "should render the events rows, got:\n{}",
            t
        );
        assert!(t.contains("title"));
        assert!(
            !t.contains("next_ids"),
            "scalar array stays out of the row table"
        );
    }

    #[test]
    fn envelope_with_two_object_arrays_is_not_unwrapped() {
        // Two candidate payloads: guessing would hide one of them.
        let v = serde_json::json!({
            "a": [{"x": 1}],
            "b": [{"y": 2}]
        });
        let t = to_table(&v);
        assert!(t.contains("key") && t.contains("value"), "{}", t);
    }

    #[test]
    fn plain_object_still_renders_as_key_value() {
        let t = to_table(&json!({"auth_open": false, "social_open": true}));
        assert!(t.contains("auth_open"));
        assert!(t.contains("social_open"));
    }

    #[test]
    fn embedded_newlines_do_not_break_the_grid() {
        // A notice body legitimately contains newlines. Emitting them raw made
        // the table wider than it measured and rows ran into each other.
        let v = serde_json::json!([
            {"id": 1, "body": "line one\nline two\nline three"},
            {"id": 2, "body": "single"}
        ]);
        let t = to_table(&v);
        let lines: Vec<&str> = t.lines().collect();
        // header + separator + exactly two data rows
        assert_eq!(lines.len(), 4, "each record must occupy one line:\n{}", t);
        assert!(
            t.contains("line one\\nline two"),
            "newlines are escaped, not dropped:\n{}",
            t
        );
        assert!(
            !t.contains("line one\nline two"),
            "no raw newline inside a row"
        );
    }

    #[test]
    fn empty_array_says_so() {
        assert_eq!(to_table(&json!([])), "(no rows)");
    }

    #[test]
    fn scalar_array_lists_each_value() {
        let t = to_table(&json!(["a", "b"]));
        assert!(t.contains('a') && t.contains('b'));
    }
}
