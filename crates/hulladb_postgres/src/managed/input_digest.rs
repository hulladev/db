//! Stable invocation input binding without retaining the submitted JSON.
use ring::digest::{Context, SHA256};
use serde_json::Value;

pub(super) fn input_digest(input: &Value) -> [u8; 32] {
    let mut digest = Context::new(&SHA256);
    append(&mut digest, input);
    digest.finish().as_ref().try_into().expect("SHA-256 length")
}

fn append(digest: &mut Context, value: &Value) {
    match value {
        Value::Object(object) => {
            digest.update(b"{");
            // Do not depend on serde_json's optional preserve_order feature.
            let mut fields: Vec<_> = object.iter().collect();
            fields.sort_unstable_by_key(|(key, _)| *key);
            for (index, (key, value)) in fields.into_iter().enumerate() {
                if index > 0 {
                    digest.update(b",");
                }
                digest.update(serde_json::to_string(key).expect("JSON string").as_bytes());
                digest.update(b":");
                append(digest, value);
            }
            digest.update(b"}");
        }
        Value::Array(values) => {
            digest.update(b"[");
            for (index, value) in values.iter().enumerate() {
                if index > 0 {
                    digest.update(b",");
                }
                append(digest, value);
            }
            digest.update(b"]");
        }
        // serde_json considers floating-point -0.0 and +0.0 equal. Preserve
        // that equality, while keeping integer zero distinct from float zero.
        Value::Number(number) if number.is_f64() && number.as_f64() == Some(0.0) => {
            digest.update(b"0.0");
        }
        scalar => digest.update(
            serde_json::to_string(scalar)
                .expect("JSON scalar")
                .as_bytes(),
        ),
    }
}

#[cfg(test)]
#[path = "../../tests/unit/input_digest.rs"]
mod tests;
