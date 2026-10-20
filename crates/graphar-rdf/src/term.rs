//! How a literal is stored: the column type of each datatype, and whether a lexical form is the
//! one the column gives back.

use crate::XSD;
use crate::schema::Storage;

/// The column type that holds literals of `datatype` and gives each lexical form back.
///
/// Typed are the datatypes whose canonical lexical form a column's natural text is, in every SQL
/// engine an RML processor reads through and in [`crate::read()`]: `xsd:integer` and `xsd:long`
/// (`int64`), `xsd:int` (`int32`), `xsd:boolean` and `xsd:date` without a time zone. Everything
/// else is kept as text with its datatype in the mapping: a float or a decimal has more than one
/// text per value, and engines disagree on which one a cast writes.
pub(crate) fn storage_of(datatype: &str) -> Storage {
    match datatype.strip_prefix(XSD) {
        Some("integer" | "long") => Storage::Int64,
        Some("int") => Storage::Int32,
        Some("boolean") => Storage::Bool,
        Some("date") => Storage::Date,
        _ => Storage::String,
    }
}

/// A typed value parsed from a lexical form that is canonical for its column.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Value {
    Text(String),
    Int64(i64),
    Int32(i32),
    Bool(bool),
    /// Days since 1970-01-01.
    Date(i32),
}

/// `lexical` as a value of `storage`, if it is the text that value gives back.
pub(crate) fn parse(storage: Storage, lexical: &str) -> Option<Value> {
    let value = match storage {
        Storage::String | Storage::LangString => return Some(Value::Text(lexical.to_owned())),
        Storage::Int64 => Value::Int64(lexical.parse().ok()?),
        Storage::Int32 => Value::Int32(lexical.parse().ok()?),
        Storage::Bool => match lexical {
            "true" => Value::Bool(true),
            "false" => Value::Bool(false),
            _ => return None,
        },
        Storage::Date => Value::Date(date(lexical)?),
    };
    // `+1`, `01` and `-0` parse, and are not what the column gives back.
    (lexical_of(&value) == lexical).then_some(value)
}

/// The lexical form of a stored value.
pub(crate) fn lexical_of(value: &Value) -> String {
    match value {
        Value::Text(t) => t.clone(),
        Value::Int64(i) => i.to_string(),
        Value::Int32(i) => i.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Date(days) => civil(*days),
    }
}

/// `YYYY-MM-DD`, years 0001 to 9999 and no time zone, as days since the epoch.
fn date(text: &str) -> Option<i32> {
    let b = text.as_bytes();
    if b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
        return None;
    }
    let num = |r: std::ops::Range<usize>| -> Option<i64> {
        text.get(r.clone())
            .filter(|s| s.bytes().all(|c| c.is_ascii_digit()))?
            .parse()
            .ok()
    };
    let (y, m, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let days_in = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    if y == 0 || !(1..=12).contains(&m) || d < 1 || d > days_in[(m - 1) as usize] {
        return None;
    }
    i32::try_from(days_from_civil(y, m, d)).ok()
}

/// Howard Hinnant's `days_from_civil`.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Howard Hinnant's `civil_from_days`, as `YYYY-MM-DD`.
pub(crate) fn civil(days: i32) -> String {
    let z = i64::from(days) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_canonical_form_is_typed() {
        assert_eq!(parse(Storage::Int64, "42"), Some(Value::Int64(42)));
        assert_eq!(parse(Storage::Int64, "-7"), Some(Value::Int64(-7)));
        for not in ["+1", "01", "-0", "1.0", " 1", "99999999999999999999"] {
            assert_eq!(parse(Storage::Int64, not), None, "{not}");
        }
        assert_eq!(parse(Storage::Int32, "3000000000"), None);
        assert_eq!(parse(Storage::Bool, "1"), None);
        assert_eq!(parse(Storage::Bool, "true"), Some(Value::Bool(true)));
        assert_eq!(parse(Storage::Date, "1970-01-01"), Some(Value::Date(0)));
        assert_eq!(
            parse(Storage::Date, "2024-02-29"),
            Some(Value::Date(19_782))
        );
        for not in [
            "2023-02-29",
            "2024-02-29Z",
            "0000-01-01",
            "24-01-01",
            "2024-1-01",
        ] {
            assert_eq!(parse(Storage::Date, not), None, "{not}");
        }
    }

    #[test]
    fn a_date_reads_back_as_itself_across_four_centuries() {
        for days in (-719_162..2_932_896).step_by(997) {
            assert_eq!(date(&civil(days)), Some(days));
        }
    }
}
