use hulladb_schema::{
    FieldDefinition, InsertKeyPolicy, KeyGeneration, ScalarType, SchemaError, TableDeclaration,
};

fn table(key_type: ScalarType) -> TableDeclaration {
    TableDeclaration::new("tasks", vec![FieldDefinition::new("id", key_type)])
}

#[test]
fn uuid_keys_default_to_v7_regardless_of_field_name_or_injection() {
    let implicit = TableDeclaration::new("tasks", vec![]);
    let declared = table(ScalarType::Uuid);
    let mut selected = TableDeclaration::new(
        "tasks",
        vec![FieldDefinition::new("task_key", ScalarType::Uuid)],
    );
    selected.primary_key = Some("task_key".into());

    for declaration in [implicit, declared, selected] {
        let contract = declaration.normalize().unwrap();
        assert_eq!(contract.key_generation(), &KeyGeneration::UuidV7);
        assert_eq!(contract.insert_key_policy(), InsertKeyPolicy::AllowExplicit);
    }
}

#[test]
fn built_in_uuid_generators_reject_incompatible_key_types() {
    for key_type in [ScalarType::String, ScalarType::Int64] {
        for generator in [KeyGeneration::UuidV7, KeyGeneration::UuidV4] {
            let mut declaration = table(key_type);
            declaration.key_generation = Some(generator);
            let error = declaration.normalize().unwrap_err();
            assert_eq!(
                error,
                SchemaError::IncompatibleKeyGenerator {
                    table: "tasks".into(),
                    field: "id".into(),
                    key_type,
                    output_type: ScalarType::Uuid,
                }
            );
            assert!(
                error
                    .to_string()
                    .contains("configure a compatible generator")
            );
        }
    }
}

#[test]
fn custom_generator_metadata_is_preserved_for_matching_keys() {
    for key_type in [ScalarType::Uuid, ScalarType::String, ScalarType::Int64] {
        for policy in [
            InsertKeyPolicy::AllowExplicit,
            InsertKeyPolicy::GeneratedOnly,
        ] {
            let generator = KeyGeneration::Custom {
                handler: "tasks.new_key".into(),
                output_type: key_type,
            };
            let mut declaration = table(key_type);
            declaration.key_generation = Some(generator.clone());
            declaration.insert_key_policy = policy;
            let contract = declaration.normalize().unwrap();
            assert_eq!(contract.key_generation(), &generator);
            assert_eq!(contract.insert_key_policy(), policy);
        }
    }
}

#[test]
fn custom_generator_must_declare_a_matching_output_type() {
    let mut declaration = table(ScalarType::Int64);
    declaration.key_generation = Some(KeyGeneration::Custom {
        handler: "tasks.new_key".into(),
        output_type: ScalarType::String,
    });
    assert_eq!(
        declaration.normalize().unwrap_err(),
        SchemaError::IncompatibleKeyGenerator {
            table: "tasks".into(),
            field: "id".into(),
            key_type: ScalarType::Int64,
            output_type: ScalarType::String,
        }
    );
}

#[test]
fn custom_generator_handler_name_cannot_be_empty() {
    let mut declaration = table(ScalarType::String);
    declaration.key_generation = Some(KeyGeneration::Custom {
        handler: String::new(),
        output_type: ScalarType::String,
    });
    assert_eq!(
        declaration.normalize().unwrap_err(),
        SchemaError::EmptyGeneratorHandler {
            table: "tasks".into(),
        }
    );
}

#[test]
fn generated_only_policy_requires_a_generator() {
    for key_type in [ScalarType::String, ScalarType::Int64, ScalarType::Uuid] {
        let mut declaration = table(key_type);
        declaration.key_generation = Some(KeyGeneration::None);
        declaration.insert_key_policy = InsertKeyPolicy::GeneratedOnly;
        let error = declaration.normalize().unwrap_err();
        assert_eq!(
            error,
            SchemaError::MissingKeyGenerator {
                table: "tasks".into(),
                field: "id".into(),
            }
        );
        assert!(
            error
                .to_string()
                .contains("configure one or allow explicit keys")
        );
    }

    let mut declaration = table(ScalarType::Int64);
    declaration.insert_key_policy = InsertKeyPolicy::GeneratedOnly;
    assert!(matches!(
        declaration.normalize(),
        Err(SchemaError::MissingKeyGenerator { .. })
    ));
}

#[test]
fn generator_validation_uses_the_selected_key_and_preserves_other_fields() {
    let fields = vec![
        FieldDefinition::new("id", ScalarType::Uuid),
        FieldDefinition::new("slug", ScalarType::String),
    ];
    let mut declaration = TableDeclaration::new("tasks", fields.clone());
    declaration.primary_key = Some("slug".into());
    let contract = declaration.clone().normalize().unwrap();
    assert_eq!(contract.fields(), fields);
    assert_eq!(contract.key_generation(), &KeyGeneration::None);

    declaration.key_generation = Some(KeyGeneration::UuidV7);
    assert_eq!(
        declaration.normalize().unwrap_err(),
        SchemaError::IncompatibleKeyGenerator {
            table: "tasks".into(),
            field: "slug".into(),
            key_type: ScalarType::String,
            output_type: ScalarType::Uuid,
        }
    );
}

#[test]
fn boolean_keys_are_rejected_for_default_and_explicit_selection() {
    for name in ["id", "is_active"] {
        for generation in [
            None,
            Some(KeyGeneration::None),
            Some(KeyGeneration::UuidV7),
            Some(KeyGeneration::Custom {
                handler: "tasks.new_key".into(),
                output_type: ScalarType::Boolean,
            }),
        ] {
            for policy in [
                InsertKeyPolicy::AllowExplicit,
                InsertKeyPolicy::GeneratedOnly,
            ] {
                let mut declaration = TableDeclaration::new(
                    "tasks",
                    vec![FieldDefinition::new(name, ScalarType::Boolean)],
                );
                if name != "id" {
                    declaration.primary_key = Some(name.into());
                }
                declaration.key_generation = generation.clone();
                declaration.insert_key_policy = policy;
                let error = declaration.normalize().unwrap_err();
                assert_eq!(
                    error,
                    SchemaError::UnsupportedPrimaryKeyType {
                        table: "tasks".into(),
                        field: name.into(),
                        scalar_type: ScalarType::Boolean,
                    }
                );
                assert!(
                    error
                        .to_string()
                        .contains("use UUID, string, or signed 64-bit integer")
                );
            }
        }
    }
}

#[test]
fn boolean_fields_remain_valid_including_an_id_that_is_not_the_key() {
    for key_type in [ScalarType::Uuid, ScalarType::String, ScalarType::Int64] {
        let fields = vec![
            FieldDefinition::new("id", ScalarType::Boolean),
            FieldDefinition::new("key", key_type),
            FieldDefinition::new("completed", ScalarType::Boolean),
        ];
        let mut declaration = TableDeclaration::new("tasks", fields.clone());
        declaration.primary_key = Some("key".into());
        let contract = declaration.normalize().unwrap();
        assert_eq!(contract.fields(), fields);
        assert_eq!(contract.primary_key().scalar_type, key_type);
    }
}
