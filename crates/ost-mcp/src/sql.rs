//! Running SQL against the mounted store and shaping results as JSON.

use duckdb::types::{TimeUnit, Value};
use duckdb::Connection;
use ost::props::format_time_us;
use serde_json::{Map, Value as J};

/// Statements a caller is allowed to run.
///
/// This is a guardrail, not a sandbox — the real protection is
/// `enable_external_access=false`, set when the connection is opened, which is
/// what stops DuckDB reading or writing files. The keyword check exists so that
/// a model asking for data cannot accidentally mutate the in-memory catalog the
/// views live in.
const READ_ONLY_STARTS: [&str; 8] = [
    "select", "with", "describe", "summarize", "show", "explain", "pragma", "table",
];

/// Reject anything that is not a single read-only statement.
pub fn check_read_only(sql: &str) -> Result<(), String> {
    let trimmed = sql.trim();
    let first = trimmed
        .split(|c: char| c.is_whitespace() || c == '(')
        .find(|w| !w.is_empty())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if !READ_ONLY_STARTS.contains(&first.as_str()) {
        return Err(format!(
            "only read-only statements are allowed; this one starts with `{first}`"
        ));
    }
    // One statement per call, so a leading SELECT cannot smuggle a second verb.
    if trimmed.trim_end_matches(';').contains(';') {
        return Err("only one statement per call".into());
    }
    Ok(())
}

pub struct Rows {
    pub columns: Vec<String>,
    pub rows: Vec<J>,
    /// True when the query produced more rows than `limit`.
    pub truncated: bool,
}

/// Run `sql` and collect at most `limit` rows as JSON objects.
pub fn query<P: duckdb::Params>(
    conn: &Connection,
    sql: &str,
    params: P,
    limit: usize,
) -> duckdb::Result<Rows> {
    let mut stmt = conn.prepare(sql)?;
    let mut rows = stmt.query(params)?;
    let columns = rows
        .as_ref()
        .map(|s| s.column_names())
        .unwrap_or_default();
    let mut out = Vec::new();
    let mut truncated = false;
    while let Some(row) = rows.next()? {
        if out.len() >= limit {
            truncated = true;
            break;
        }
        let mut obj = Map::with_capacity(columns.len());
        for (i, name) in columns.iter().enumerate() {
            obj.insert(name.clone(), to_json(row.get::<usize, Value>(i)?));
        }
        out.push(J::Object(obj));
    }
    Ok(Rows {
        columns,
        rows: out,
        truncated,
    })
}

/// A timestamp becomes `YYYY-MM-DD HH:MM:SS` (UTC) rather than a bare integer,
/// because that is what a model can reason about without being told the unit.
fn to_json(v: Value) -> J {
    match v {
        Value::Null => J::Null,
        Value::Boolean(b) => J::Bool(b),
        Value::TinyInt(n) => n.into(),
        Value::SmallInt(n) => n.into(),
        Value::Int(n) => n.into(),
        Value::BigInt(n) => n.into(),
        Value::UTinyInt(n) => n.into(),
        Value::USmallInt(n) => n.into(),
        Value::UInt(n) => n.into(),
        Value::UBigInt(n) => n.into(),
        // `sum()` over a BIGINT column returns HUGEINT, so this is a normal
        // aggregate result, not an exotic one.
        Value::HugeInt(n) => i128_json(n),
        Value::UHugeInt(n) => i128::try_from(n).map(i128_json).unwrap_or(J::Null),
        Value::Float(n) => J::from(n as f64),
        Value::Double(n) => J::from(n),
        Value::Text(s) => J::String(s),
        Value::Timestamp(unit, n) => J::String(format_time_us(match unit {
            TimeUnit::Second => n.saturating_mul(1_000_000),
            TimeUnit::Millisecond => n.saturating_mul(1_000),
            TimeUnit::Microsecond => n,
            TimeUnit::Nanosecond => n / 1_000,
        })),
        Value::Blob(b) => J::String(format!("<{} bytes>", b.len())),
        other => J::String(format!("{other:?}")),
    }
}

/// JSON has no 128-bit integer, so anything outside `i64` becomes a string
/// rather than a silently rounded float.
fn i128_json(n: i128) -> J {
    match i64::try_from(n) {
        Ok(v) => v.into(),
        Err(_) => J::String(n.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_only_check() {
        assert!(check_read_only("SELECT 1").is_ok());
        assert!(check_read_only("  with x as (select 1) select * from x").is_ok());
        assert!(check_read_only("(SELECT 1) UNION (SELECT 2)").is_ok());
        assert!(check_read_only("select 1;").is_ok());
        assert!(check_read_only("CREATE TABLE t (a INT)").is_err());
        assert!(check_read_only("COPY messages TO 'out.csv'").is_err());
        assert!(check_read_only("select 1; drop view messages").is_err());
    }
}
