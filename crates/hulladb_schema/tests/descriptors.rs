use hulladb_schema::{KeyGeneration, ScalarType, Value, descriptor::schema_from_descriptor};
use serde_json::{Value as Json, json};

// Independent authoring input: do not regenerate this fixture with the encoder under test.
fn descriptor() -> Json {
    json!({
        "uuid_default": "V7",
        "tables": [{
            "name": "notes", "key": "key", "generation": "None",
            "insert_policy": "AllowExplicit", "unique": [], "indexes": [], "checks": [],
            "fields": [
                { "name": "key", "scalar": "String", "nullable": false, "default": null, "constraints": { "oneOf": [] } },
                { "name": "count", "scalar": "Int64", "nullable": false, "default": ["int64", "9223372036854775807"], "constraints": { "oneOf": [] } },
                { "name": "score", "scalar": "Float64", "nullable": true, "default": ["float64", 1], "constraints": { "oneOf": [], "min": ["float64", 0] } }
            ]
        }],
        "references": []
    })
}

#[test]
fn independent_authoring_descriptor_preserves_exact_defaults_and_key_contract() {
    let schema = schema_from_descriptor(descriptor()).unwrap();
    let table = schema.table("notes").unwrap();
    assert_eq!(table.primary_key().name, "key");
    assert_eq!(table.primary_key().scalar_type, ScalarType::String);
    assert_eq!(table.key_generation(), &KeyGeneration::None);
    assert_eq!(
        table.field("count").unwrap().default,
        Some(Value::Int64(i64::MAX))
    );
    assert!(table.field("id").is_none());
    // This guards the installed PostgreSQL contract while moving its encoder into shared code.
    assert!(hulladb_schema::descriptor::descriptor_equal(
        &schema.descriptor(),
        &descriptor()
    ));
}

#[test]
fn malformed_defaults_and_normalized_contract_disagreements_are_rejected() {
    for default in [
        json!(["int64", "01"]),
        json!(["int64", "9223372036854775808"]),
        json!(["int64", 12]),
    ] {
        let mut input = descriptor();
        input["tables"][0]["fields"][1]["default"] = default;
        assert!(schema_from_descriptor(input).is_err());
    }
    let mut input = descriptor();
    input["tables"][0]["fields"][2]["default"] = json!(["float64", -1]);
    assert!(schema_from_descriptor(input).is_err());
    let mut input = descriptor();
    input["references"] = json!([{"table":"notes","field":"key","target":"notes","key":"count","onDelete":"Restrict"}]);
    assert!(schema_from_descriptor(input).is_err());
}

#[test]
fn shared_metadata_accepts_named_generators_without_executing_application_code() {
    let mut input = descriptor();
    input["tables"][0]["generation"] = json!({"custom":"notes.next_key", "output":"String"});
    let schema = schema_from_descriptor(input).unwrap();
    assert_eq!(
        schema.table("notes").unwrap().key_generation(),
        &KeyGeneration::Custom {
            handler: "notes.next_key".into(),
            output_type: ScalarType::String
        }
    );
}
