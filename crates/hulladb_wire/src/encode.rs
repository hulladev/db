use std::fmt::Write;

use hulladb_schema::{RecordValues, Value};
use serde::ser::{Serialize, SerializeMap, Serializer};

use crate::WireError;

/// Encode a logical record deterministically in lexicographic field-name order.
/// Integers use canonical decimal text; UUIDs use lowercase hyphenated text.
/// This does not validate the record against a table or apply insert defaults.
pub fn encode_record(values: &RecordValues) -> Result<String, WireError> {
    serde_json::to_string(&WireRecord(values)).map_err(Into::into)
}

struct WireRecord<'a>(&'a RecordValues);

impl Serialize for WireRecord<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (field, value) in self.0 {
            map.serialize_entry(field, &WireScalar(value))?;
        }
        map.end()
    }
}

struct WireScalar<'a>(&'a Value);

impl Serialize for WireScalar<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self.0 {
            Value::Null => serializer.serialize_unit(),
            Value::String(value) => serializer.serialize_str(value),
            Value::Boolean(value) => serializer.serialize_bool(*value),
            Value::Int64(value) => {
                let mut map = serializer.serialize_map(Some(1))?;
                map.serialize_entry("$int64", &value.to_string())?;
                map.end()
            }
            Value::Uuid(value) => {
                let mut text = String::with_capacity(36);
                for (index, byte) in value.iter().enumerate() {
                    if [4, 6, 8, 10].contains(&index) {
                        text.push('-');
                    }
                    // Writing to a String cannot fail.
                    write!(text, "{byte:02x}").map_err(serde::ser::Error::custom)?;
                }
                let mut map = serializer.serialize_map(Some(1))?;
                map.serialize_entry("$uuid", &text)?;
                map.end()
            }
        }
    }
}
