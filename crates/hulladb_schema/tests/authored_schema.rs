use hulladb_schema::{
    CheckOperand, InsertKeyAction, InsertKeyPolicy, KeyGeneration, RecordValues, ReferenceOnDelete,
    ScalarType, UniqueNullPolicy, UuidGeneration, Value,
};

mod generated {
    include!("fixtures/articles_schema.rs");
}

#[test]
fn typescript_authored_schema_normalizes_through_native_contracts() {
    let schema = generated::schema().unwrap();
    assert_eq!(schema.default_uuid_generation(), UuidGeneration::V4);
    let publications = schema.table("publications").unwrap();
    assert_eq!(publications.primary_key().name, "slug");
    assert!(publications.field("id").is_none());
    assert_eq!(publications.key_generation(), &KeyGeneration::None);
    let articles = schema.table("articles").unwrap();
    assert_eq!(articles.primary_key().scalar_type, ScalarType::Uuid);
    assert_eq!(articles.key_generation(), &KeyGeneration::UuidV4);
    assert_eq!(articles.insert_key_policy(), InsertKeyPolicy::GeneratedOnly);
    assert_eq!(
        articles.unique_constraints()[0].fields(),
        &["publication", "title"]
    );
    assert_eq!(
        articles.unique_constraints()[1].null_policy(),
        UniqueNullPolicy::Distinct
    );
    let reference = &schema.references()[0];
    assert_eq!(reference.table(), "articles");
    assert_eq!(reference.field(), "publication");
    assert_eq!(reference.target_table(), "publications");
    assert_eq!(reference.target_field(), "slug");
    assert_eq!(reference.scalar_type(), ScalarType::String);
    assert_eq!(reference.on_delete(), ReferenceOnDelete::Cascade);
    assert_eq!(articles.indexes()[0].fields, ["publication", "published"]);
    assert_eq!(
        articles.checks()[0].operands[3],
        CheckOperand::Value(Value::Int64(0))
    );
    assert_eq!(
        schema.table("counters").unwrap().key_generation(),
        &KeyGeneration::Custom {
            handler: "counters.next".into(),
            output_type: ScalarType::Int64,
        }
    );
}

#[test]
fn generated_schema_preserves_lossless_defaults_and_native_write_validation() {
    let schema = generated::schema().unwrap();
    let articles = schema.table("articles").unwrap();
    let input = RecordValues::from([
        ("publication".into(), Value::String("daily".into())),
        ("title".into(), Value::String("First".into())),
        ("excerpt".into(), Value::Null),
    ]);
    let prepared = articles.prepare_insert(input.clone()).unwrap();
    assert_eq!(prepared.key_action(), InsertKeyAction::Generate);
    let mut row = prepared.into_values();
    assert_eq!(row.get("published"), Some(&Value::Boolean(false)));
    assert_eq!(row.get("ordinal"), Some(&Value::Int64(i64::MAX)));
    assert_eq!(row.get("minimum"), Some(&Value::Int64(i64::MIN)));
    assert_eq!(
        row.get("label"),
        Some(&Value::String("line\n\"quote\"\\path\0🦀".into()))
    );
    assert_eq!(
        row.get("origin"),
        Some(&Value::Uuid([
            0xaa, 0xaa, 0xaa, 0xaa, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1
        ]))
    );
    assert!(articles.validate_row(&row).is_err());
    row.insert("id".into(), Value::Uuid([2; 16]));
    articles.validate_row(&row).unwrap();
    assert!(
        articles
            .validate_update(&RecordValues::from([(
                "title".into(),
                Value::String(String::new())
            )]))
            .is_err()
    );
    assert!(articles.validate_insert(&row).is_err()); // Generated-only policy.
    assert!(
        articles
            .validate_update(&RecordValues::from([("id".into(), Value::Uuid([3; 16]))]))
            .is_err()
    );
    let mut missing_nullable = input;
    missing_nullable.remove("excerpt");
    assert!(articles.prepare_insert(missing_nullable).is_err());
    assert!(
        schema
            .table("publications")
            .unwrap()
            .validate_insert(&RecordValues::from([(
                "title".into(),
                Value::String("Daily".into())
            )]))
            .is_err()
    );
}
