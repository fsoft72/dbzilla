//! Text to typed value conversion for imports.

use dbzilla_core::value::Value;

/// Converts a cell of an import file to a value of the destination column type.
pub fn to_value(type_name: &str, text: Option<&str>) -> Result<Value, String> {
    let Some(raw) = text else { return Ok(Value::Null) };
    let t = type_name.to_lowercase();
    let s = raw.trim();
    let is_int = t.contains("int") || t.contains("serial");
    let is_dec = t.contains("numeric") || t.contains("decimal") || t.contains("money");
    let is_float = t.contains("float") || t.contains("double") || t.contains("real");
    if is_int || is_dec || is_float {
        if s.is_empty() {
            return Ok(Value::Null);
        }
        return if is_int {
            s.parse::<i64>().map(Value::I64).map_err(|_| format!("'{s}' is not a whole number"))
        } else if is_dec {
            s.parse::<f64>().map(|_| Value::Decimal(s.to_owned())).map_err(|_| format!("'{s}' is not a number"))
        } else {
            s.parse::<f64>().map(Value::F64).map_err(|_| format!("'{s}' is not a number"))
        };
    }
    if t == "bool" || t == "boolean" {
        return match s.to_lowercase().as_str() {
            "true" | "t" | "1" | "yes" => Ok(Value::Bool(true)),
            "false" | "f" | "0" | "no" => Ok(Value::Bool(false)),
            "" => Ok(Value::Null),
            other => Err(format!("'{other}' is not true or false")),
        };
    }
    Ok(Value::Text(raw.to_owned()))
}
