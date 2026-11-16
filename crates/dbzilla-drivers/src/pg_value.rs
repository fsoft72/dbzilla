//! Binary decoding of PostgreSQL values into `Value`.

use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, Utc};
use dbzilla_core::value::Value;
use std::error::Error;
use tokio_postgres::types::{FromSql, Kind, Type};

/// Boxed error used by `FromSql`.
type DecodeError = Box<dyn Error + Sync + Send>;

/// Wrapper accepting every PostgreSQL type and decoding it to a `Value`.
pub struct PgValue(pub Value);

impl<'a> FromSql<'a> for PgValue {
    fn from_sql(ty: &Type, raw: &'a [u8]) -> Result<Self, DecodeError> {
        decode(ty, raw).map(PgValue)
    }

    fn from_sql_null(_ty: &Type) -> Result<Self, DecodeError> {
        Ok(PgValue(Value::Null))
    }

    fn accepts(_ty: &Type) -> bool {
        true
    }
}

/// Extension types whose binary form is plain UTF-8 text.
const TEXT_EXTENSIONS: [&str; 3] = ["citext", "ltree", "lquery"];

/// Reads a big-endian i32 at `pos`.
fn i32_at(raw: &[u8], pos: usize) -> Result<i32, DecodeError> {
    let bytes: [u8; 4] = raw.get(pos..pos + 4).ok_or("value is too short")?.try_into()?;
    Ok(i32::from_be_bytes(bytes))
}

/// Reads a big-endian i64 at `pos`.
fn i64_at(raw: &[u8], pos: usize) -> Result<i64, DecodeError> {
    let bytes: [u8; 8] = raw.get(pos..pos + 8).ok_or("value is too short")?.try_into()?;
    Ok(i64::from_be_bytes(bytes))
}

/// Reads a big-endian u16 at `pos`.
fn u16_at(raw: &[u8], pos: usize) -> Result<u16, DecodeError> {
    let bytes: [u8; 2] = raw.get(pos..pos + 2).ok_or("value is too short")?.try_into()?;
    Ok(u16::from_be_bytes(bytes))
}

/// Hex text of bytes, used as a last resort.
fn hex(raw: &[u8]) -> String {
    raw.iter().map(|b| format!("{b:02x}")).collect()
}

/// Decodes one binary value according to its type.
pub fn decode(ty: &Type, raw: &[u8]) -> Result<Value, DecodeError> {
    match ty.kind() {
        Kind::Array(inner) => return decode_array(inner, raw),
        Kind::Domain(inner) => return decode(inner, raw),
        Kind::Enum(_) => return Ok(Value::Text(std::str::from_utf8(raw)?.to_owned())),
        _ => {}
    }
    Ok(match *ty {
        Type::BOOL => Value::Bool(raw.first().copied().ok_or("empty bool")? != 0),
        Type::INT2 => Value::I64(i64::from(i16::from_be_bytes(raw.try_into()?))),
        Type::INT4 => Value::I64(i64::from(i32::from_be_bytes(raw.try_into()?))),
        Type::INT8 => Value::I64(i64::from_be_bytes(raw.try_into()?)),
        Type::OID | Type::XID | Type::CID => Value::I64(i64::from(u32::from_be_bytes(raw.try_into()?))),
        Type::FLOAT4 => {
            let f = f32::from_be_bytes(raw.try_into()?);
            Value::F64(f.to_string().parse::<f64>().unwrap_or(f64::from(f)))
        }
        Type::FLOAT8 => Value::F64(f64::from_be_bytes(raw.try_into()?)),
        Type::NUMERIC => Value::Decimal(numeric_to_string(raw)?),
        Type::TEXT | Type::VARCHAR | Type::BPCHAR | Type::NAME | Type::XML | Type::UNKNOWN => Value::Text(std::str::from_utf8(raw)?.to_owned()),
        Type::CHAR => Value::Text(raw.iter().map(|&b| b as char).collect()),
        Type::BYTEA => Value::Bytes(raw.to_vec()),
        Type::UUID => Value::Uuid(uuid::Uuid::from_slice(raw)?),
        Type::JSON | Type::JSONB => Value::Json(serde_json::Value::from_sql(ty, raw)?),
        Type::DATE => match i32_at(raw, 0)? {
            i32::MAX => Value::Other("infinity".into()),
            i32::MIN => Value::Other("-infinity".into()),
            _ => Value::Date(NaiveDate::from_sql(ty, raw)?),
        },
        Type::TIME => Value::Time(NaiveTime::from_sql(ty, raw)?),
        Type::TIMETZ => Value::Other(timetz(raw)?),
        Type::TIMESTAMP => match i64_at(raw, 0)? {
            i64::MAX => Value::Other("infinity".into()),
            i64::MIN => Value::Other("-infinity".into()),
            _ => Value::DateTime(NaiveDateTime::from_sql(ty, raw)?),
        },
        Type::TIMESTAMPTZ => match i64_at(raw, 0)? {
            i64::MAX => Value::Other("infinity".into()),
            i64::MIN => Value::Other("-infinity".into()),
            _ => Value::DateTimeTz(DateTime::<Utc>::from_sql(ty, raw)?.fixed_offset()),
        },
        Type::INTERVAL => Value::Other(interval(raw)?),
        Type::INET | Type::CIDR => Value::Other(inet(raw)?),
        Type::MACADDR => Value::Other(raw.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(":")),
        Type::MONEY => {
            let cents = i64_at(raw, 0)?;
            Value::Other(format!("{}{}.{:02}", if cents < 0 { "-" } else { "" }, cents.unsigned_abs() / 100, cents.unsigned_abs() % 100))
        }
        Type::BIT | Type::VARBIT => Value::Other(bits(raw)?),
        _ => match std::str::from_utf8(raw) {
            Ok(text) if TEXT_EXTENSIONS.contains(&ty.name()) => Value::Text(text.to_owned()),
            Ok(text) => Value::Other(text.to_owned()),
            Err(_) => Value::Other(format!("\\x{}", hex(raw))),
        },
    })
}

/// Decodes the binary array format into nested `Value::Array`.
fn decode_array(inner: &Type, raw: &[u8]) -> Result<Value, DecodeError> {
    let ndim = i32_at(raw, 0)?;
    if ndim <= 0 {
        return Ok(Value::Array(vec![]));
    }
    let mut pos = 12;
    let mut dims = Vec::new();
    for _ in 0..ndim {
        dims.push(usize::try_from(i32_at(raw, pos)?)?);
        pos += 8;
    }
    let total: usize = dims.iter().product();
    let mut values = Vec::with_capacity(total);
    for _ in 0..total {
        let len = i32_at(raw, pos)?;
        pos += 4;
        if len < 0 {
            values.push(Value::Null);
            continue;
        }
        let len = len as usize;
        values.push(decode(inner, raw.get(pos..pos + len).ok_or("array element is too short")?)?);
        pos += len;
    }
    Ok(nest(values, &dims))
}

/// Groups a flat list into nested arrays following `dims`.
fn nest(values: Vec<Value>, dims: &[usize]) -> Value {
    if dims.len() <= 1 {
        return Value::Array(values);
    }
    let chunk: usize = dims[1..].iter().product::<usize>().max(1);
    Value::Array(values.chunks(chunk).map(|c| nest(c.to_vec(), &dims[1..])).collect())
}

/// Decodes the binary NUMERIC format to its decimal text.
fn numeric_to_string(raw: &[u8]) -> Result<String, DecodeError> {
    let ndigits = usize::from(u16_at(raw, 0)?);
    let weight = i32::from(u16_at(raw, 2)? as i16);
    let sign = u16_at(raw, 4)?;
    let dscale = usize::from(u16_at(raw, 6)?);
    match sign {
        0xC000 => return Ok("NaN".into()),
        0xD000 => return Ok("Infinity".into()),
        0xF000 => return Ok("-Infinity".into()),
        _ => {}
    }
    let digit = |i: i32| -> Result<u16, DecodeError> { if i >= 0 && (i as usize) < ndigits { u16_at(raw, 8 + 2 * i as usize) } else { Ok(0) } };
    let mut int_part = String::new();
    if weight < 0 {
        int_part.push('0');
    } else {
        for i in 0..=weight {
            let d = digit(i)?;
            if i == 0 { int_part.push_str(&d.to_string()) } else { int_part.push_str(&format!("{d:04}")) }
        }
    }
    let mut frac = String::new();
    let mut g = weight + 1;
    while frac.len() < dscale {
        frac.push_str(&format!("{:04}", digit(g)?));
        g += 1;
    }
    frac.truncate(dscale);
    let negative = sign == 0x4000;
    let mut out = String::new();
    if negative {
        out.push('-');
    }
    out.push_str(&int_part);
    if dscale > 0 {
        out.push('.');
        out.push_str(&frac);
    }
    Ok(out)
}

/// Formats a PostgreSQL interval like the server does.
fn interval(raw: &[u8]) -> Result<String, DecodeError> {
    let micros = i64_at(raw, 0)?;
    let days = i32_at(raw, 8)?;
    let months = i32_at(raw, 12)?;
    let plural = |n: i32, one: &str, many: &str| format!("{n} {}", if n.abs() == 1 { one } else { many });
    let mut parts = Vec::new();
    if months / 12 != 0 {
        parts.push(plural(months / 12, "year", "years"));
    }
    if months % 12 != 0 {
        parts.push(plural(months % 12, "mon", "mons"));
    }
    if days != 0 {
        parts.push(plural(days, "day", "days"));
    }
    if micros != 0 || parts.is_empty() {
        let abs = micros.unsigned_abs();
        let (h, m, s, frac) = (abs / 3_600_000_000, abs / 60_000_000 % 60, abs / 1_000_000 % 60, abs % 1_000_000);
        let mut t = format!("{}{h:02}:{m:02}:{s:02}", if micros < 0 { "-" } else { "" });
        if frac != 0 {
            let digits = format!("{frac:06}");
            t.push('.');
            t.push_str(digits.trim_end_matches('0'));
        }
        parts.push(t);
    }
    Ok(parts.join(" "))
}

/// Formats a `timetz` value.
fn timetz(raw: &[u8]) -> Result<String, DecodeError> {
    let micros = i64_at(raw, 0)?;
    let zone = -i32_at(raw, 8)?;
    let secs = micros / 1_000_000;
    let sign = if zone < 0 { '-' } else { '+' };
    let z = zone.abs();
    Ok(format!("{:02}:{:02}:{:02}{sign}{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60, z / 3600, z / 60 % 60))
}

/// Formats an `inet` / `cidr` value.
fn inet(raw: &[u8]) -> Result<String, DecodeError> {
    let family = *raw.first().ok_or("empty inet")?;
    let bits = *raw.get(1).ok_or("short inet")?;
    let is_cidr = *raw.get(2).ok_or("short inet")? != 0;
    let addr = raw.get(4..).ok_or("short inet")?;
    let (text, full) = match family {
        2 => {
            let a: [u8; 4] = addr.try_into()?;
            (std::net::Ipv4Addr::from(a).to_string(), 32)
        }
        3 => {
            let a: [u8; 16] = addr.try_into()?;
            (std::net::Ipv6Addr::from(a).to_string(), 128)
        }
        other => return Err(format!("unknown address family {other}").into()),
    };
    Ok(if is_cidr || bits != full { format!("{text}/{bits}") } else { text })
}

/// Formats `bit` / `varbit` as a string of 0 and 1.
fn bits(raw: &[u8]) -> Result<String, DecodeError> {
    let n = usize::try_from(i32_at(raw, 0)?)?;
    let data = raw.get(4..).ok_or("short bit string")?;
    Ok((0..n).map(|i| if data.get(i / 8).is_some_and(|b| b >> (7 - i % 8) & 1 == 1) { '1' } else { '0' }).collect())
}
