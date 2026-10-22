use std::fmt;

use hulladb_schema::{RecordValues, Value};
use serde::de::{Deserialize, Deserializer, MapAccess, Visitor};
use serde_json::value::RawValue;

use crate::{WireError, WireErrorKind};

/// Decode exactly one JSON record, preserving types and rejecting duplicate
/// decoded field names before they can overwrite a map entry.
///
/// JSON parsing precedes field checks, which follow input order. No partial
/// record is returned on failure. Schema validation is a separate step.
pub fn decode_record(input: &str) -> Result<RecordValues, WireError> {
    let raw: RawObject = serde_json::from_str(input)?;
    let mut values = RecordValues::new();
    for (field, raw_value) in raw.0 {
        if values.contains_key(&field) {
            return Err(WireError::Field {
                field,
                kind: WireErrorKind::DuplicateField,
            });
        }
        let value = decode_scalar(raw_value.get()).map_err(|kind| WireError::Field {
            field: field.clone(),
            kind,
        })?;
        values.insert(field, value);
    }
    Ok(values)
}

// A Vec preserves repeated keys, including escaped spellings of the same key.
// RawValue retains exact JSON tokens without rounding numbers or constructing
// nested maps that would discard duplicate scalar tags.
struct RawObject(Vec<(String, Box<RawValue>)>);

impl<'de> Deserialize<'de> for RawObject {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ObjectVisitor;
        impl<'de> Visitor<'de> for ObjectVisitor {
            type Value = RawObject;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a JSON record object")
            }

            fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Self::Value, M::Error> {
                let mut entries = Vec::new();
                while let Some(entry) = map.next_entry()? {
                    entries.push(entry);
                }
                Ok(RawObject(entries))
            }
        }
        deserializer.deserialize_map(ObjectVisitor)
    }
}

fn decode_scalar(raw: &str) -> Result<Value, WireErrorKind> {
    // RawValue has already checked JSON syntax. Parsing only the allowed shapes
    // keeps ordinary strings distinct from explicitly tagged integers/UUIDs.
    match raw.as_bytes().first() {
        Some(b'n') => Ok(Value::Null),
        Some(b't' | b'f') => serde_json::from_str(raw)
            .map(Value::Boolean)
            .map_err(|_| WireErrorKind::UnsupportedValue),
        Some(b'"') => serde_json::from_str(raw)
            .map(Value::String)
            .map_err(|_| WireErrorKind::UnsupportedValue),
        Some(b'{') => {
            let mut object: RawObject =
                serde_json::from_str(raw).map_err(|_| WireErrorKind::InvalidScalarTag)?;
            if object.0.len() != 1 {
                return Err(WireErrorKind::InvalidScalarTag);
            }
            let (tag, payload) = object.0.pop().ok_or(WireErrorKind::InvalidScalarTag)?;
            if tag == "$json" {
                let value = serde_json::from_str(payload.get())
                    .map_err(|_| WireErrorKind::UnsupportedValue)?;
                hulladb_schema::JsonShape::Any
                    .validate(&value)
                    .map_err(|_| WireErrorKind::UnsupportedValue)?;
                return Ok(Value::Json(value));
            }
            let text: String =
                serde_json::from_str(payload.get()).map_err(|_| WireErrorKind::InvalidScalarTag)?;
            match tag.as_str() {
                "$date" if hulladb_schema::temporal::date(&text).is_some() => Ok(Value::Date(text)),
                "$timestamp" if hulladb_schema::temporal::timestamp(&text, 6, true).is_some() => {
                    Ok(Value::Timestamp(text))
                }
                "$localTimestamp"
                    if hulladb_schema::temporal::timestamp(&text, 6, false).is_some() =>
                {
                    Ok(Value::LocalTimestamp(text))
                }
                "$int64" => decode_int64(&text).map(Value::Int64),
                "$uuid" => decode_uuid(&text).map(Value::Uuid),
                _ => Err(WireErrorKind::InvalidScalarTag),
            }
        }
        Some(b'-' | b'0'..=b'9') => serde_json::from_str(raw)
            .map(Value::Float64)
            .map_err(|_| WireErrorKind::UnsupportedValue),
        _ => Err(WireErrorKind::UnsupportedValue),
    }
}

fn decode_int64(text: &str) -> Result<i64, WireErrorKind> {
    let value: i64 = text.parse().map_err(|_| WireErrorKind::InvalidInt64)?;
    // Reject +1, -0, leading zeroes, whitespace, exponents, and other aliases.
    if value.to_string() != text {
        return Err(WireErrorKind::InvalidInt64);
    }
    Ok(value)
}

fn decode_uuid(text: &str) -> Result<[u8; 16], WireErrorKind> {
    let bytes = text.as_bytes();
    if bytes.len() != 36 {
        return Err(WireErrorKind::InvalidUuid);
    }
    let mut uuid = [0; 16];
    let mut cursor = 0;
    for (index, byte) in uuid.iter_mut().enumerate() {
        if [4, 6, 8, 10].contains(&index) {
            if bytes[cursor] != b'-' {
                return Err(WireErrorKind::InvalidUuid);
            }
            cursor += 1;
        }
        let high = hex_digit(bytes[cursor]).ok_or(WireErrorKind::InvalidUuid)?;
        let low = hex_digit(bytes[cursor + 1]).ok_or(WireErrorKind::InvalidUuid)?;
        *byte = high * 16 + low;
        cursor += 2;
    }
    Ok(uuid)
}

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}
