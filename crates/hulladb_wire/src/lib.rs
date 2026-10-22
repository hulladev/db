//! Lossless JSON records for the current Hulla DB scalar contract.
//!
//! Strings, booleans, and null use native JSON values. Integers use
//! `{"$int64":"9223372036854775807"}` and UUIDs use
//! `{"$uuid":"00112233-4455-6677-8899-aabbccddeeff"}`. Finite JSON numbers
//! represent float64 values. Date/time strings use `$date`, `$timestamp`, and `$localTimestamp` tags.
//! Structured JSON uses a `$json` tag. Untagged arrays and objects are rejected.
//! Omission remains absence of a field.
//!
//! Decoding checks wire representation, not table shape, permissions, or storage
//! constraints. Pass the decoded record to the appropriate schema validator.
//! The format is provisional; transport envelopes and version negotiation are
//! subsequent protocol work.
//!
//! ```
//! use hulladb_schema::{FieldDefinition, ScalarType, TableDeclaration, Value};
//! use hulladb_wire::{decode_record, encode_record};
//!
//! let table = TableDeclaration::new("tasks", vec![
//!     FieldDefinition::new("id", ScalarType::Int64),
//!     FieldDefinition::new("title", ScalarType::String),
//! ]).normalize()?;
//! let values = decode_record(r#"{"id":{"$int64":"9223372036854775807"},"title":"Plan"}"#)?;
//! assert_eq!(values.get("id"), Some(&Value::Int64(i64::MAX)));
//! table.validate_insert(&values)?;
//! assert_eq!(decode_record(&encode_record(&values)?)?, values);
//! assert!(decode_record(r#"{"title":"first","title":"second"}"#).is_err());
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

mod decode;
mod encode;
mod error;

pub use decode::decode_record;
pub use encode::encode_record;
pub use error::{WireError, WireErrorKind};
