use hulladb_schema::{FieldDefinition, ScalarType, Value};
use serde_json::{Map, Value as Json};
use uuid::Uuid;

use crate::Error;

/// Portable function value codec. Signed int64 uses decimal text, UUID uses
/// hyphenated text, matching the deployed function SDK (not the record codec).
pub trait Codec: Sized + Send + Sync + 'static {
    fn encode(&self) -> Result<Json, Error>;
    fn decode(value: &Json) -> Result<Self, Error>;
}

pub trait Input: Codec {
    fn fields() -> Vec<FieldDefinition>;
}

/// The canonical scalar subset available to table/input derives.
pub trait Scalar: Codec + Clone {
    const TYPE: ScalarType;
    const NULLABLE: bool = false;
    fn value(&self) -> Value;
}

impl Codec for String {
    fn encode(&self) -> Result<Json, Error> {
        Ok(Json::String(self.clone()))
    }
    fn decode(value: &Json) -> Result<Self, Error> {
        value
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| invalid("expected a string"))
    }
}
impl Scalar for String {
    const TYPE: ScalarType = ScalarType::String;
    fn value(&self) -> Value {
        Value::String(self.clone())
    }
}
impl Codec for bool {
    fn encode(&self) -> Result<Json, Error> {
        Ok(Json::Bool(*self))
    }
    fn decode(value: &Json) -> Result<Self, Error> {
        value.as_bool().ok_or_else(|| invalid("expected a boolean"))
    }
}
impl Scalar for bool {
    const TYPE: ScalarType = ScalarType::Boolean;
    fn value(&self) -> Value {
        Value::Boolean(*self)
    }
}
impl Codec for i64 {
    fn encode(&self) -> Result<Json, Error> {
        Ok(Json::String(self.to_string()))
    }
    fn decode(value: &Json) -> Result<Self, Error> {
        let text = value
            .as_str()
            .ok_or_else(|| invalid("expected canonical int64 decimal text"))?;
        let value: i64 = text.parse().map_err(|_| invalid("int64 is out of range"))?;
        if value.to_string() != text {
            return Err(invalid("int64 text is not canonical"));
        }
        Ok(value)
    }
}
impl Scalar for i64 {
    const TYPE: ScalarType = ScalarType::Int64;
    fn value(&self) -> Value {
        Value::Int64(*self)
    }
}
impl Codec for Uuid {
    fn encode(&self) -> Result<Json, Error> {
        Ok(Json::String(self.to_string()))
    }
    fn decode(value: &Json) -> Result<Self, Error> {
        let text = value
            .as_str()
            .ok_or_else(|| invalid("expected a UUID string"))?;
        let id = Uuid::parse_str(text).map_err(|_| invalid("invalid UUID"))?;
        if text.len() != 36 || id.to_string() != text.to_ascii_lowercase() {
            return Err(invalid("expected a hyphenated UUID"));
        }
        Ok(id)
    }
}
impl Scalar for Uuid {
    const TYPE: ScalarType = ScalarType::Uuid;
    fn value(&self) -> Value {
        Value::Uuid(*self.as_bytes())
    }
}
impl<T: Codec> Codec for Option<T> {
    fn encode(&self) -> Result<Json, Error> {
        self.as_ref().map_or(Ok(Json::Null), Codec::encode)
    }
    fn decode(value: &Json) -> Result<Self, Error> {
        if value.is_null() {
            Ok(None)
        } else {
            T::decode(value).map(Some)
        }
    }
}
impl<T: Scalar> Scalar for Option<T> {
    const TYPE: ScalarType = T::TYPE;
    const NULLABLE: bool = true;
    fn value(&self) -> Value {
        self.as_ref().map_or(Value::Null, Scalar::value)
    }
}
impl<T: Codec> Codec for Vec<T> {
    fn encode(&self) -> Result<Json, Error> {
        self.iter()
            .map(Codec::encode)
            .collect::<Result<Vec<_>, _>>()
            .map(Json::Array)
    }
    fn decode(value: &Json) -> Result<Self, Error> {
        value
            .as_array()
            .ok_or_else(|| invalid("expected an array"))?
            .iter()
            .map(T::decode)
            .collect()
    }
}
impl Codec for () {
    fn encode(&self) -> Result<Json, Error> {
        Ok(Json::Null)
    }
    fn decode(value: &Json) -> Result<Self, Error> {
        if value.is_null() {
            Ok(())
        } else {
            Err(invalid("this function takes no input"))
        }
    }
}

pub fn object(value: &Json) -> Result<Map<String, Json>, Error> {
    value
        .as_object()
        .cloned()
        .ok_or_else(|| invalid("expected a closed record"))
}
pub fn required(object: &mut Map<String, Json>, name: &str) -> Result<Json, Error> {
    object
        .remove(name)
        .ok_or_else(|| invalid(&format!("missing field {name:?}")))
}
pub fn reject_unknown(object: &Map<String, Json>) -> Result<(), Error> {
    if let Some(name) = object.keys().next() {
        Err(invalid(&format!("unknown field {name:?}")))
    } else {
        Ok(())
    }
}
fn invalid(message: &str) -> Error {
    Error::InvalidInput(message.into())
}
