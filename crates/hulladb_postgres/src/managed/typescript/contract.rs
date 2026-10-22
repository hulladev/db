use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value as Json;

use super::super::Error;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum Shape {
    String,
    Json,
    Number,
    Boolean,
    Bigint,
    Date,
    Null,
    Undefined,
    Literal { value: Json },
    Array { item: Box<Shape> },
    Tuple { items: Vec<Shape> },
    Union { choices: Vec<Shape> },
    Object { fields: BTreeMap<String, Field> },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Field {
    pub shape: Shape,
    pub optional: bool,
}

// Preserve semantic differences during union decoding, especially string/bigint
// and null/undefined. The browser codec rejects ambiguous wire representations.
#[derive(Debug, PartialEq)]
enum Decoded {
    Json(Json),
    Number(f64),
    Bigint(String),
    Date(i64),
    Undefined,
    Array(Vec<Self>),
    Object(BTreeMap<String, Self>),
}

pub fn int64(value: &Json) -> Result<i64, Error> {
    let text = value.as_str().ok_or_else(invalid)?;
    let number = text.parse::<i64>().map_err(|_| invalid())?;
    if number.to_string() != text {
        return Err(invalid());
    }
    Ok(number)
}

fn invalid() -> Error {
    Error::InvalidInput("value violates the generated TypeScript contract".into())
}

impl Shape {
    pub fn validate(&self, value: &Json) -> Result<(), Error> {
        self.decode(value, 0).map(|_| ())
    }

    fn decode(&self, value: &Json, depth: usize) -> Result<Decoded, Error> {
        if depth > 128 {
            return Err(invalid());
        }
        let primitive = match self {
            Self::Json => {
                hulladb_schema::JsonShape::Any
                    .validate(value)
                    .map_err(|_| invalid())?;
                return Ok(decode_json(value));
            }
            Self::String => value.is_string(),
            Self::Boolean => value.is_boolean(),
            Self::Null => value.is_null(),
            Self::Number => value.as_f64().is_some_and(f64::is_finite),
            Self::Literal { value: literal } => {
                (literal.is_string() || literal.is_boolean() || literal.is_number())
                    && (value == literal
                        || (value.is_number()
                            && literal.is_number()
                            && value.as_f64() == literal.as_f64()))
            }
            Self::Bigint => {
                let text = value.as_str().ok_or_else(invalid)?;
                let digits = text.strip_prefix('-').unwrap_or(text);
                if text != "0"
                    && (digits.is_empty()
                        || !matches!(digits.as_bytes()[0], b'1'..=b'9')
                        || !digits.bytes().all(|byte| byte.is_ascii_digit()))
                {
                    return Err(invalid());
                }
                return Ok(Decoded::Bigint(text.to_owned()));
            }
            Self::Date => {
                let fields = value
                    .as_object()
                    .filter(|fields| fields.len() == 1)
                    .ok_or_else(invalid)?;
                let number = fields
                    .get("$date")
                    .and_then(Json::as_f64)
                    .ok_or_else(invalid)?;
                if !number.is_finite()
                    || number.fract() != 0.0
                    || number.abs() > 8_640_000_000_000_000.0
                {
                    return Err(invalid());
                }
                return Ok(Decoded::Date(number as i64));
            }
            Self::Undefined if value.is_null() => return Ok(Decoded::Undefined),
            Self::Undefined => false,
            Self::Array { item } => {
                return value
                    .as_array()
                    .ok_or_else(invalid)?
                    .iter()
                    .map(|value| item.decode(value, depth + 1))
                    .collect::<Result<Vec<_>, _>>()
                    .map(Decoded::Array);
            }
            Self::Tuple { items } => {
                let values = value
                    .as_array()
                    .filter(|values| values.len() == items.len())
                    .ok_or_else(invalid)?;
                return items
                    .iter()
                    .zip(values)
                    .map(|(shape, value)| shape.decode(value, depth + 1))
                    .collect::<Result<Vec<_>, _>>()
                    .map(Decoded::Array);
            }
            Self::Object { fields } => {
                let values = value.as_object().ok_or_else(invalid)?;
                if values.keys().any(|key| !fields.contains_key(key)) {
                    return Err(invalid());
                }
                let mut result = BTreeMap::new();
                for (key, field) in fields {
                    match values.get(key) {
                        Some(value) => {
                            result.insert(key.clone(), field.shape.decode(value, depth + 1)?);
                        }
                        None if field.optional => {}
                        None => return Err(invalid()),
                    }
                }
                return Ok(Decoded::Object(result));
            }
            Self::Union { choices } => {
                let mut matches = choices
                    .iter()
                    .filter_map(|shape| shape.decode(value, depth + 1).ok());
                let first = matches.next().ok_or_else(invalid)?;
                if matches.any(|other| other != first) {
                    return Err(invalid());
                }
                return Ok(first);
            }
        };
        if primitive {
            Ok(if let Some(number) = value.as_f64() {
                Decoded::Number(number)
            } else {
                Decoded::Json(value.clone())
            })
        } else {
            Err(invalid())
        }
    }
}

fn decode_json(value: &Json) -> Decoded {
    match value {
        Json::Array(items) => Decoded::Array(items.iter().map(decode_json).collect()),
        Json::Object(fields) => Decoded::Object(
            fields
                .iter()
                .map(|(k, v)| (k.clone(), decode_json(v)))
                .collect(),
        ),
        Json::Number(number) => Decoded::Number(number.as_f64().expect("finite JSON number")),
        _ => Decoded::Json(value.clone()),
    }
}
