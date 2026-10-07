use hulladb_schema::{
    FieldDefinition, InsertKeyAction, InsertKeyPolicy, KeyGeneration, RecordValues,
    ReferenceDeclaration, ScalarType, SchemaDeclaration, SchemaError, TableDeclaration,
    UuidGeneration, Value,
};

fn uuid_tables() -> Vec<TableDeclaration> {
    let mut custom = TableDeclaration::new(
        "custom",
        vec![FieldDefinition::new("key", ScalarType::Uuid)],
    );
    custom.primary_key = Some("key".into());
    vec![
        TableDeclaration::new("implicit", vec![]),
        TableDeclaration::new(
            "declared",
            vec![FieldDefinition::new("id", ScalarType::Uuid)],
        ),
        custom,
    ]
}

#[test]
fn schema_default_applies_to_implicit_declared_and_custom_named_uuid_keys() {
    assert_eq!(
        SchemaDeclaration::new(vec![]).default_uuid_generation,
        UuidGeneration::V7
    );
    for (generation, expected) in [
        (UuidGeneration::V4, KeyGeneration::UuidV4),
        (UuidGeneration::V7, KeyGeneration::UuidV7),
    ] {
        let mut declaration = SchemaDeclaration::new(uuid_tables());
        declaration.default_uuid_generation = generation;
        for table in &mut declaration.tables {
            table.insert_key_policy = InsertKeyPolicy::GeneratedOnly;
        }
        let contract = declaration.normalize().unwrap();
        assert_eq!(contract.default_uuid_generation(), generation);
        for table in contract.tables() {
            assert_eq!(table.key_generation(), &expected);
            assert_eq!(
                table.validate_insert(&RecordValues::new()),
                Ok(InsertKeyAction::Generate)
            );
        }
    }
}

#[test]
fn explicit_table_generators_and_disabled_generation_override_either_default() {
    for generation in [UuidGeneration::V4, UuidGeneration::V7] {
        for override_generator in [
            KeyGeneration::None,
            KeyGeneration::UuidV4,
            KeyGeneration::UuidV7,
            KeyGeneration::Custom {
                handler: "tasks.key".into(),
                output_type: ScalarType::Uuid,
            },
        ] {
            let mut tables = uuid_tables();
            for table in &mut tables {
                table.key_generation = Some(override_generator.clone());
            }
            let mut declaration = SchemaDeclaration::new(tables);
            declaration.default_uuid_generation = generation;
            let contract = declaration.normalize().unwrap();
            for table in contract.tables() {
                assert_eq!(table.key_generation(), &override_generator);
            }
        }
    }
}

#[test]
fn uuid_default_does_not_generate_non_uuid_keys_or_hide_invalid_overrides() {
    for generation in [UuidGeneration::V4, UuidGeneration::V7] {
        for scalar_type in [ScalarType::String, ScalarType::Int64] {
            let table =
                TableDeclaration::new("tasks", vec![FieldDefinition::new("id", scalar_type)]);
            let mut declaration = SchemaDeclaration::new(vec![table]);
            declaration.default_uuid_generation = generation;
            let contract = declaration.clone().normalize().unwrap();
            assert_eq!(
                contract.table("tasks").unwrap().key_generation(),
                &KeyGeneration::None
            );
            assert!(
                contract
                    .table("tasks")
                    .unwrap()
                    .validate_insert(&RecordValues::new())
                    .is_err()
            );
            declaration.tables[0].key_generation = Some(KeyGeneration::UuidV4);
            assert_eq!(
                declaration.normalize().unwrap_err(),
                SchemaError::IncompatibleKeyGenerator {
                    table: "tasks".into(),
                    field: "id".into(),
                    key_type: scalar_type,
                    output_type: ScalarType::Uuid,
                }
            );
        }
        let mut table = TableDeclaration::new("tasks", vec![]);
        table.key_generation = Some(KeyGeneration::None);
        table.insert_key_policy = InsertKeyPolicy::GeneratedOnly;
        let mut declaration = SchemaDeclaration::new(vec![table]);
        declaration.default_uuid_generation = generation;
        assert_eq!(
            declaration.normalize().unwrap_err(),
            SchemaError::MissingKeyGenerator {
                table: "tasks".into(),
                field: "id".into(),
            }
        );
    }
}

#[test]
fn switching_uuid_default_preserves_references_and_supplied_uuid_values() {
    let tasks = TableDeclaration::new(
        "tasks",
        vec![FieldDefinition::new("projectId", ScalarType::Uuid)],
    );
    let mut declaration =
        SchemaDeclaration::new(vec![tasks, TableDeclaration::new("projects", vec![])]);
    declaration
        .references
        .push(ReferenceDeclaration::new("tasks", "projectId", "projects"));
    let v7 = declaration.clone().normalize().unwrap();
    declaration.default_uuid_generation = UuidGeneration::V4;
    let v4 = declaration.normalize().unwrap();
    assert_eq!(v4.references(), v7.references());
    assert_eq!(
        v4.table("tasks").unwrap().fields(),
        v7.table("tasks").unwrap().fields()
    );
    for version in [4, 7] {
        let mut uuid = [1; 16];
        uuid[6] = version << 4 | 1;
        uuid[8] = 0x81;
        let row = RecordValues::from([("id".into(), Value::Uuid(uuid))]);
        for contract in [&v4, &v7] {
            let table = contract.table("projects").unwrap();
            assert_eq!(table.validate_insert(&row), Ok(InsertKeyAction::Provided));
            assert!(table.validate_row(&row).is_ok());
        }
    }
}
