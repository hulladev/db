use hulladb_schema::{
    FieldDefinition, JsonProperty, JsonShape, RecordValues, ScalarType, TableDeclaration,
    ValidationErrorKind, Value,
};
use serde_json::json;
use std::collections::BTreeMap;

#[test]
fn strict_nested_json_replacement_rejects_unknown_missing_and_wrong_elements() {
    let mut field = FieldDefinition::new("settings", ScalarType::Json);
    field.json_shape = Some(JsonShape::Object {
        fields: BTreeMap::from([
            (
                "tags".into(),
                JsonProperty {
                    optional: false,
                    shape: JsonShape::Array {
                        item: Box::new(JsonShape::Nullable {
                            inner: Box::new(JsonShape::String),
                        }),
                    },
                },
            ),
            (
                "theme".into(),
                JsonProperty {
                    optional: true,
                    shape: JsonShape::String,
                },
            ),
        ]),
    });
    let table = TableDeclaration::new("users", vec![field])
        .normalize()
        .unwrap();
    let values = |v| RecordValues::from([("settings".into(), Value::Json(v))]);
    table
        .validate_update(&values(json!({"tags":["urgent",null]})))
        .unwrap();
    for invalid in [
        json!({"tags":[true]}),
        json!({}),
        json!({"tags":[],"unknown":true}),
    ] {
        assert!(matches!(
            table.validate_update(&values(invalid)).unwrap_err().kind,
            ValidationErrorKind::InvalidJson { .. }
        ));
    }
    let mut row = values(json!({"tags":["old"],"theme":"dark"}));
    row.insert("id".into(), Value::Uuid([1; 16]));
    table
        .apply_update(&mut row, values(json!({"tags":[]})))
        .unwrap();
    assert_eq!(row["settings"], Value::Json(json!({"tags":[]})));
    assert!(table.validate_update(&values(json!(null))).is_err());
}

#[test]
fn unrestricted_json_and_shapes_have_bounded_depth_and_json_cannot_be_a_key() {
    let mut nested = json!(true);
    for _ in 0..66 {
        nested = json!([nested]);
    }
    assert!(JsonShape::Any.validate(&nested).is_err());
    let mut shape = JsonShape::String;
    for _ in 0..66 {
        shape = JsonShape::Array {
            item: Box::new(shape),
        };
    }
    let mut field = FieldDefinition::new("data", ScalarType::Json);
    field.json_shape = Some(shape);
    assert!(
        TableDeclaration::new("rows", vec![field])
            .normalize()
            .is_err()
    );
    assert!(
        TableDeclaration::new("rows", vec![FieldDefinition::new("id", ScalarType::Json)])
            .normalize()
            .is_err()
    );
}
