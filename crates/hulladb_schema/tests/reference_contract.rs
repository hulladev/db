use hulladb_schema::{
    FieldDefinition, RecordValues, ReferenceDeclaration, ScalarType, SchemaDeclaration,
    SchemaError, TableDeclaration, Value,
};

fn schema(scalar_type: ScalarType) -> SchemaDeclaration {
    let tasks = TableDeclaration::new(
        "tasks",
        vec![FieldDefinition::new("projectId", scalar_type)],
    );
    let mut projects =
        TableDeclaration::new("projects", vec![FieldDefinition::new("slug", scalar_type)]);
    projects.primary_key = Some("slug".into());
    let mut declaration = SchemaDeclaration::new(vec![tasks, projects]);
    declaration.references = vec![ReferenceDeclaration::new("tasks", "projectId", "projects")];
    declaration
}

#[test]
fn references_resolve_forward_targets_and_selected_key_types() {
    for scalar_type in [ScalarType::String, ScalarType::Int64, ScalarType::Uuid] {
        let declaration = schema(scalar_type);
        let contract = declaration.clone().normalize().unwrap();
        assert_eq!(contract, declaration.normalize().unwrap());
        let reference = &contract.references()[0];
        assert_eq!(reference.table(), "tasks");
        assert_eq!(reference.field(), "projectId");
        assert_eq!(reference.target_table(), "projects");
        assert_eq!(reference.target_field(), "slug");
        assert_eq!(reference.scalar_type(), scalar_type);
        assert!(!reference.nullable());
        assert!(contract.table("projects").unwrap().field("id").is_none());
        assert_eq!(contract.tables()[0].name(), "tasks");
        assert_eq!(contract.tables()[1].name(), "projects");
        let cloned = contract.clone();
        assert_eq!(cloned, contract);
        for table in cloned.tables() {
            assert!(std::ptr::eq(cloned.table(table.name()).unwrap(), table));
        }
        assert!(cloned.table("absent").is_none());
    }
}

#[test]
fn references_resolve_injected_keys_and_preserve_explicit_order() {
    let mut declaration = SchemaDeclaration::new(vec![
        TableDeclaration::new("projects", vec![]),
        TableDeclaration::new("users", vec![]),
        TableDeclaration::new(
            "tasks",
            vec![
                FieldDefinition::new("projectId", ScalarType::Uuid),
                FieldDefinition::new("assigneeId", ScalarType::Uuid),
            ],
        ),
    ]);
    declaration.references = vec![
        ReferenceDeclaration::new("tasks", "assigneeId", "users"),
        ReferenceDeclaration::new("tasks", "projectId", "projects"),
    ];
    let contract = declaration.normalize().unwrap();
    assert_eq!(contract.references()[0].field(), "assigneeId");
    assert_eq!(contract.references()[1].field(), "projectId");
    assert!(
        contract
            .references()
            .iter()
            .all(|reference| reference.target_field() == "id")
    );
}

#[test]
fn nullable_references_keep_existing_null_and_omission_rules() {
    let mut declaration = schema(ScalarType::String);
    declaration.tables[0].fields[0].nullable = true;
    declaration.tables[0].fields[0].default = Some(Value::Null);
    let contract = declaration.normalize().unwrap();
    assert!(contract.references()[0].nullable());
    let table = contract.table("tasks").unwrap();
    let prepared = table.prepare_insert(RecordValues::new()).unwrap();
    assert_eq!(prepared.values().get("projectId"), Some(&Value::Null));
    assert!(table.validate_row(prepared.values()).is_err());
    table
        .validate_update(&RecordValues::from([("projectId".into(), Value::Null)]))
        .unwrap();
}

#[test]
fn matching_names_do_not_create_implicit_references() {
    let mut declaration = schema(ScalarType::String);
    declaration.references.clear();
    assert!(declaration.normalize().unwrap().references().is_empty());
    assert!(
        SchemaDeclaration::new(vec![])
            .normalize()
            .unwrap()
            .tables()
            .is_empty()
    );
}

#[test]
fn duplicate_table_names_and_invalid_table_declarations_are_rejected() {
    let mut declaration = schema(ScalarType::String);
    declaration.tables.push(declaration.tables[0].clone());
    assert_eq!(
        declaration.normalize().unwrap_err(),
        SchemaError::DuplicateTable {
            table: "tasks".into()
        }
    );
    let mut declaration = schema(ScalarType::String);
    declaration.tables[0].fields[0].name.clear();
    assert_eq!(
        declaration.normalize().unwrap_err(),
        SchemaError::EmptyFieldName {
            table: "tasks".into()
        }
    );
}

#[test]
fn missing_or_empty_source_tables_and_fields_are_rejected() {
    for name in ["missing", ""] {
        let mut declaration = schema(ScalarType::String);
        declaration.references[0].table = name.into();
        assert_eq!(
            declaration.normalize().unwrap_err(),
            SchemaError::MissingReferenceSourceTable {
                table: name.into(),
                field: "projectId".into(),
            }
        );
        let mut declaration = schema(ScalarType::String);
        declaration.references[0].field = name.into();
        assert_eq!(
            declaration.normalize().unwrap_err(),
            SchemaError::MissingReferenceSourceField {
                table: "tasks".into(),
                field: name.into(),
            }
        );
    }
}

#[test]
fn missing_or_empty_target_tables_are_rejected() {
    for name in ["missing", ""] {
        let mut declaration = schema(ScalarType::String);
        declaration.references[0].target_table = name.into();
        let error = declaration.normalize().unwrap_err();
        assert_eq!(
            error,
            SchemaError::MissingReferenceTargetTable {
                table: "tasks".into(),
                field: "projectId".into(),
                target_table: name.into(),
            }
        );
        assert!(error.to_string().contains("references undeclared table"));
    }
}

#[test]
fn repeated_or_conflicting_references_from_one_field_are_rejected() {
    for target in ["projects", "other"] {
        let mut declaration = schema(ScalarType::String);
        declaration.tables.push(TableDeclaration::new(
            "other",
            vec![FieldDefinition::new("id", ScalarType::String)],
        ));
        declaration
            .references
            .push(ReferenceDeclaration::new("tasks", "projectId", target));
        assert_eq!(
            declaration.normalize().unwrap_err(),
            SchemaError::DuplicateReference {
                table: "tasks".into(),
                field: "projectId".into(),
            }
        );
    }
}

#[test]
fn reference_type_mismatches_are_rejected_even_when_nullable() {
    for nullable in [false, true] {
        let mut declaration = schema(ScalarType::String);
        declaration.tables[0].fields[0].scalar_type = ScalarType::Int64;
        declaration.tables[0].fields[0].nullable = nullable;
        let error = declaration.normalize().unwrap_err();
        assert_eq!(
            error,
            SchemaError::IncompatibleReferenceType {
                table: "tasks".into(),
                field: "projectId".into(),
                target_table: "projects".into(),
                field_type: ScalarType::Int64,
                key_type: ScalarType::String,
            }
        );
        assert!(error.to_string().contains("key type String"));
    }
}

#[test]
fn self_and_cyclic_references_resolve_without_recursive_normalization() {
    let mut parent = FieldDefinition::new("parentId", ScalarType::Uuid);
    parent.nullable = true;
    let mut declaration = SchemaDeclaration::new(vec![
        TableDeclaration::new(
            "a",
            vec![parent, FieldDefinition::new("bId", ScalarType::Uuid)],
        ),
        TableDeclaration::new("b", vec![FieldDefinition::new("aId", ScalarType::Uuid)]),
    ]);
    declaration.references = vec![
        ReferenceDeclaration::new("a", "parentId", "a"),
        ReferenceDeclaration::new("a", "bId", "b"),
        ReferenceDeclaration::new("b", "aId", "a"),
    ];
    let contract = declaration.normalize().unwrap();
    assert_eq!(contract.references().len(), 3);
    assert!(contract.references()[0].nullable());
    // Reordering tables changes presentation order, not reference resolution.
    let mut declaration = SchemaDeclaration::new(vec![
        TableDeclaration::new("b", vec![FieldDefinition::new("aId", ScalarType::Uuid)]),
        TableDeclaration::new("a", vec![FieldDefinition::new("bId", ScalarType::Uuid)]),
    ]);
    declaration.references = vec![
        ReferenceDeclaration::new("a", "bId", "b"),
        ReferenceDeclaration::new("b", "aId", "a"),
    ];
    assert_eq!(
        declaration.normalize().unwrap().references(),
        &contract.references()[1..]
    );
}

#[test]
fn matching_reference_types_cannot_make_an_unsupported_target_key_valid() {
    let declaration = schema(ScalarType::Boolean);
    assert_eq!(
        declaration.normalize().unwrap_err(),
        SchemaError::UnsupportedPrimaryKeyType {
            table: "projects".into(),
            field: "slug".into(),
            scalar_type: ScalarType::Boolean,
        }
    );
}
