use hulladb_schema::{FieldDefinition, KeyGeneration, ScalarType, SchemaError, TableDeclaration};

fn table(fields: Vec<FieldDefinition>, primary_key: Option<&str>) -> TableDeclaration {
    let mut declaration = TableDeclaration::new("tasks", fields);
    declaration.primary_key = primary_key.map(str::to_owned);
    declaration
}

#[test]
fn omitted_id_is_added_for_default_or_explicit_id_selection() {
    for primary_key in [None, Some("id")] {
        let title = FieldDefinition::new("title", ScalarType::String);
        let declaration = table(vec![title.clone()], primary_key);
        let contract = declaration.normalize().unwrap();
        assert_eq!(contract.name(), "tasks");
        assert_eq!(contract.fields().len(), 2);
        assert_eq!(contract.fields()[0], title);
        assert_eq!(contract.primary_key().name, "id");
        assert_eq!(contract.primary_key().scalar_type, ScalarType::Uuid);
        assert!(!contract.primary_key().nullable);
        assert_eq!(contract.key_generation(), &KeyGeneration::UuidV7);
    }
}

#[test]
fn declared_id_remains_primary_key_and_preserves_its_type() {
    for scalar_type in [ScalarType::Uuid, ScalarType::String, ScalarType::Int64] {
        let id = FieldDefinition::new("id", scalar_type);
        let fields = vec![
            id.clone(),
            FieldDefinition::new("done", ScalarType::Boolean),
        ];
        let contract = table(fields.clone(), None).normalize().unwrap();

        assert_eq!(contract.fields(), fields);
        assert_eq!(contract.primary_key(), &id);
        let expected_generation = match scalar_type {
            ScalarType::Uuid => KeyGeneration::UuidV7,
            _ => KeyGeneration::None,
        };
        assert_eq!(contract.key_generation(), &expected_generation);
    }
}

#[test]
fn another_primary_key_does_not_inject_id() {
    let slug = FieldDefinition::new("slug", ScalarType::String);
    let contract = table(vec![slug.clone()], Some("slug")).normalize().unwrap();

    assert_eq!(contract.fields(), std::slice::from_ref(&slug));
    assert_eq!(contract.primary_key(), &slug);
    assert!(contract.field("id").is_none());
    assert_eq!(contract.key_generation(), &KeyGeneration::None);
}

#[test]
fn another_primary_key_preserves_an_explicitly_declared_id() {
    let mut id = FieldDefinition::new("id", ScalarType::Int64);
    id.nullable = true;
    let slug = FieldDefinition::new("slug", ScalarType::String);
    let fields = vec![id.clone(), slug.clone()];
    let contract = table(fields.clone(), Some("slug")).normalize().unwrap();

    assert_eq!(contract.fields(), fields);
    assert_eq!(contract.field("id"), Some(&id));
    assert_eq!(contract.primary_key(), &slug);
}

#[test]
fn custom_primary_key_must_be_declared() {
    let error = table(vec![], Some("slug")).normalize().unwrap_err();
    assert_eq!(
        error,
        SchemaError::MissingPrimaryKeyField {
            table: "tasks".into(),
            field: "slug".into(),
        }
    );
    assert!(error.to_string().contains("declare that field"));
}

#[test]
fn selected_primary_key_cannot_be_nullable() {
    for (name, selection) in [("id", None), ("id", Some("id")), ("slug", Some("slug"))] {
        let mut key = FieldDefinition::new(name, ScalarType::String);
        key.nullable = true;
        assert_eq!(
            table(vec![key], selection).normalize().unwrap_err(),
            SchemaError::NullablePrimaryKey {
                table: "tasks".into(),
                field: name.into(),
            }
        );
    }
}

#[test]
fn duplicate_fields_are_rejected_instead_of_silently_overwriting_an_id() {
    let fields = vec![
        FieldDefinition::new("id", ScalarType::Int64),
        FieldDefinition::new("id", ScalarType::Uuid),
    ];
    assert_eq!(
        table(fields, None).normalize().unwrap_err(),
        SchemaError::DuplicateField {
            table: "tasks".into(),
            field: "id".into(),
        }
    );
}

#[test]
fn empty_names_are_rejected() {
    let mut declaration = table(vec![], None);
    declaration.name.clear();
    assert_eq!(
        declaration.normalize().unwrap_err(),
        SchemaError::EmptyTableName
    );

    assert_eq!(
        table(vec![FieldDefinition::new("", ScalarType::String)], None)
            .normalize()
            .unwrap_err(),
        SchemaError::EmptyFieldName {
            table: "tasks".into(),
        }
    );

    assert_eq!(
        table(vec![], Some("")).normalize().unwrap_err(),
        SchemaError::EmptyPrimaryKeyName {
            table: "tasks".into(),
        }
    );
}

#[test]
fn field_lookup_matches_declaration_order_and_injected_fields() {
    let contract = table(
        vec![
            FieldDefinition::new("zebra", ScalarType::String),
            FieldDefinition::new("alpha", ScalarType::Boolean),
            FieldDefinition::new("middle", ScalarType::Int64),
        ],
        None,
    )
    .normalize()
    .unwrap();
    assert_eq!(
        contract
            .fields()
            .iter()
            .map(|field| field.name.as_str())
            .collect::<Vec<_>>(),
        ["zebra", "alpha", "middle", "id"]
    );
    for field in contract.fields() {
        assert_eq!(contract.field(&field.name), Some(field));
    }
    assert!(contract.field("missing").is_none());
}
