use super::input_digest;
use serde_json::{Value, json};

#[test]
fn binding_preserves_json_equality_without_ambiguous_concatenation() {
    let left: Value = serde_json::from_str(r#"{"z":[{"b":2,"a":1}],"a":-0.0}"#).unwrap();
    let right: Value = serde_json::from_str(r#"{"a":0.0,"z":[{"a":1,"b":2}]}"#).unwrap();
    assert_eq!(left, right);
    assert_eq!(input_digest(&left), input_digest(&right));

    // Preserve type, key, nesting and array-order boundaries. These must not
    // permit the same invocation identity to bind a different submitted input.
    for (left, right) in [
        (json!(["ab", "c"]), json!(["a", "bc"])),
        (json!({"ab": "c"}), json!({"a": "bc"})),
        (json!([1, 2]), json!([2, 1])),
        (json!([1]), json!(1)),
        (json!(0), json!(0.0)),
        (json!(1), json!(1.0)),
        (
            json!(9_007_199_254_740_992_u64),
            json!(9_007_199_254_740_993_u64),
        ),
        (json!("null"), Value::Null),
        (json!({}), json!({"missing": null})),
        (json!(true), json!("true")),
        (json!({"a": {"b": 1}}), json!({"a.b": 1})),
        (json!("a\"b"), json!("a\\\"b")),
    ] {
        assert_ne!(left, right);
        assert_ne!(
            input_digest(&left),
            input_digest(&right),
            "{left} versus {right}"
        );
    }
}
