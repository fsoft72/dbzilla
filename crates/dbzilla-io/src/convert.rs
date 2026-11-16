//! Text to typed value conversion for imports.

use dbzilla_core::value::Value;

/// Kind of a column type, from its declared name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeClass {
    Integer,
    Decimal,
    Float,
    Bool,
    Binary,
    Other,
}

/// Classifies a declared type by its base name (`bigint unsigned`, `numeric(10,2)`, `int4`), never by substring:
/// `point` and `interval` are not integers.
pub fn classify_type(type_name: &str) -> TypeClass {
    let lower = type_name.to_lowercase();
    let base = lower.split(['(', ' ']).next().unwrap_or("").trim_end_matches("[]");
    match base {
        "tinyint" | "smallint" | "mediumint" | "int" | "integer" | "bigint" | "int2" | "int4" | "int8" | "serial" | "smallserial" | "bigserial" | "serial2" | "serial4" | "serial8" | "year" => TypeClass::Integer,
        "numeric" | "decimal" | "dec" | "fixed" | "money" => TypeClass::Decimal,
        "float" | "double" | "real" | "float4" | "float8" => TypeClass::Float,
        "bool" | "boolean" => TypeClass::Bool,
        "blob" | "tinyblob" | "mediumblob" | "longblob" | "bytea" | "binary" | "varbinary" => TypeClass::Binary,
        _ => TypeClass::Other,
    }
}

/// Bytes of a `0x...` hex text.
fn parse_hex(s: &str) -> Result<Vec<u8>, String> {
    let digits = s.trim().trim_start_matches("0x").trim_start_matches("0X");
    if digits.len() % 2 != 0 || !digits.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(format!("'{s}' is not hexadecimal bytes (0x...)"));
    }
    Ok((0..digits.len()).step_by(2).map(|i| u8::from_str_radix(&digits[i..i + 2], 16).unwrap_or(0)).collect())
}

/// Converts a cell of an import file to a value of the destination column type.
pub fn to_value(type_name: &str, text: Option<&str>) -> Result<Value, String> {
    let Some(raw) = text else { return Ok(Value::Null) };
    let s = raw.trim();
    match classify_type(type_name) {
        TypeClass::Integer => {
            if s.is_empty() {
                return Ok(Value::Null);
            }
            if let Ok(n) = s.parse::<i64>() {
                return Ok(Value::I64(n));
            }
            s.parse::<u64>().map(Value::U64).map_err(|_| format!("'{s}' is not a whole number"))
        }
        TypeClass::Decimal => {
            if s.is_empty() {
                return Ok(Value::Null);
            }
            s.parse::<f64>().map(|_| Value::Decimal(s.to_owned())).map_err(|_| format!("'{s}' is not a number"))
        }
        TypeClass::Float => {
            if s.is_empty() {
                return Ok(Value::Null);
            }
            s.parse::<f64>().map(Value::F64).map_err(|_| format!("'{s}' is not a number"))
        }
        TypeClass::Bool => match s.to_lowercase().as_str() {
            "true" | "t" | "1" | "yes" => Ok(Value::Bool(true)),
            "false" | "f" | "0" | "no" => Ok(Value::Bool(false)),
            "" => Ok(Value::Null),
            other => Err(format!("'{other}' is not true or false")),
        },
        TypeClass::Binary if s.starts_with("0x") || s.starts_with("0X") => parse_hex(s).map(Value::Bytes),
        TypeClass::Binary | TypeClass::Other => Ok(Value::Text(raw.to_owned())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Review M10: types are classified by name, not by substring.
    #[test]
    fn classification() {
        assert_eq!(classify_type("point"), TypeClass::Other);
        assert_eq!(classify_type("interval"), TypeClass::Other);
        assert_eq!(classify_type("BIGINT UNSIGNED"), TypeClass::Integer);
        assert_eq!(classify_type("int(11)"), TypeClass::Integer);
        assert_eq!(classify_type("numeric(10,2)"), TypeClass::Decimal);
        assert_eq!(classify_type("double precision"), TypeClass::Float);
        assert_eq!(classify_type("bytea"), TypeClass::Binary);
    }

    /// Values above i64::MAX, hex bytes and non-numeric types.
    #[test]
    fn conversion() {
        assert_eq!(to_value("bigint unsigned", Some("18446744073709551615")).unwrap(), Value::U64(u64::MAX));
        assert_eq!(to_value("point", Some("(1,2)")).unwrap(), Value::Text("(1,2)".into()));
        assert_eq!(to_value("blob", Some("0xDEAD")).unwrap(), Value::Bytes(vec![0xDE, 0xAD]));
        assert!(to_value("blob", Some("0xZZ")).is_err());
        assert_eq!(to_value("int", Some("")).unwrap(), Value::Null);
        assert!(to_value("int", Some("abc")).is_err());
    }
}
