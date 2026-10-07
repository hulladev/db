use hulladb_schema::{
    FieldDefinition, InsertKeyAction, KeyGeneration, RecordValues, ReferenceDeclaration,
    ScalarType, SchemaDeclaration, SchemaError, TableDeclaration, UniqueConstraintDeclaration,
    UniqueNullPolicy, UuidGeneration, Value,
};

fn membership() -> TableDeclaration {
    let mut table = TableDeclaration::new(
        "memberships",
        vec![
            FieldDefinition::new("projectId", ScalarType::Uuid),
            FieldDefinition::new("userId", ScalarType::Uuid),
        ],
    );
    table
        .unique_constraints
        .push(unique("project_user", &["projectId", "userId"]));
    table
}

fn unique(name: &str, fields: &[&str]) -> UniqueConstraintDeclaration {
    UniqueConstraintDeclaration::new(name, fields.iter().map(|name| (*name).into()).collect())
}

#[test]
fn business_uniqueness_preserves_row_identity_and_authored_order() {
    let mut declaration = membership();
    declaration.unique_constraints[0].fields.reverse();
    declaration
        .unique_constraints
        .push(unique("one_project", &["projectId"]));
    let contract = declaration.normalize().unwrap();
    assert_eq!(contract.primary_key().name, "id");
    assert_eq!(contract.key_generation(), &KeyGeneration::UuidV7);
    assert_eq!(contract.unique_constraints()[0].name(), "project_user");
    assert_eq!(
        contract.unique_constraints()[0].fields(),
        &["userId", "projectId"]
    );
    assert_eq!(
        contract.unique_constraints()[0].null_policy(),
        UniqueNullPolicy::NotDistinct
    );
    assert_eq!(contract.unique_constraints()[1].name(), "one_project");
    assert_eq!(
        contract
            .fields()
            .iter()
            .map(|field| field.name.as_str())
            .collect::<Vec<_>>(),
        ["projectId", "userId", "id"]
    );
    assert!(
        TableDeclaration::new("empty", vec![])
            .normalize()
            .unwrap()
            .unique_constraints()
            .is_empty()
    );
}

#[test]
fn all_current_scalars_can_have_single_field_uniqueness() {
    for scalar_type in [
        ScalarType::Uuid,
        ScalarType::String,
        ScalarType::Int64,
        ScalarType::Boolean,
    ] {
        let mut declaration =
            TableDeclaration::new("items", vec![FieldDefinition::new("value", scalar_type)]);
        declaration
            .unique_constraints
            .push(unique("unique_value", &["value"]));
        let contract = declaration.normalize().unwrap();
        assert_eq!(contract.field("value").unwrap().scalar_type, scalar_type);
        assert_eq!(contract.unique_constraints()[0].fields(), &["value"]);
    }
}

#[test]
fn nullable_components_require_explicit_semantics_even_with_a_non_null_default() {
    for fields in [
        vec!["userId"],
        vec!["projectId", "userId"],
        vec!["userId", "projectId"],
    ] {
        let mut declaration = membership();
        declaration.fields[1].nullable = true;
        declaration.fields[1].default = Some(Value::Uuid([3; 16]));
        declaration.unique_constraints = vec![unique("nullable", &fields)];
        let error = declaration.normalize().unwrap_err();
        assert_eq!(
            error,
            SchemaError::MissingUniqueNullPolicy {
                table: "memberships".into(),
                constraint: "nullable".into(),
            }
        );
        assert!(
            error
                .to_string()
                .contains("explicitly choose Distinct or NotDistinct")
        );
    }
}

#[test]
fn both_null_policies_are_resolved_for_nullable_and_non_nullable_fields() {
    for nullable in [false, true] {
        for policy in [UniqueNullPolicy::Distinct, UniqueNullPolicy::NotDistinct] {
            let mut declaration = membership();
            declaration.fields[0].nullable = nullable;
            declaration.fields[1].nullable = nullable;
            declaration.unique_constraints[0].null_policy = Some(policy);
            let contract = declaration.normalize().unwrap();
            assert_eq!(contract.unique_constraints()[0].null_policy(), policy);
            assert_eq!(contract.field("projectId").unwrap().nullable, nullable);
        }
    }
}

#[test]
fn malformed_constraint_names_and_empty_field_lists_are_rejected() {
    for (name, fields, expected) in [
        (
            "",
            vec!["projectId"],
            SchemaError::EmptyUniqueConstraintName {
                table: "memberships".into(),
            },
        ),
        (
            "empty",
            vec![],
            SchemaError::EmptyUniqueConstraintFields {
                table: "memberships".into(),
                constraint: "empty".into(),
            },
        ),
    ] {
        let mut declaration = membership();
        declaration.unique_constraints = vec![unique(name, &fields)];
        assert_eq!(declaration.normalize().unwrap_err(), expected);
    }
    let mut declaration = membership();
    declaration
        .unique_constraints
        .push(unique("project_user", &["projectId"]));
    assert_eq!(
        declaration.normalize().unwrap_err(),
        SchemaError::DuplicateUniqueConstraintName {
            table: "memberships".into(),
            constraint: "project_user".into(),
        }
    );
}

#[test]
fn missing_and_repeated_fields_cannot_silently_change_uniqueness() {
    for missing in ["", "missing", "ProjectId"] {
        let mut declaration = membership();
        declaration.unique_constraints[0]
            .fields
            .push(missing.into());
        assert_eq!(
            declaration.normalize().unwrap_err(),
            SchemaError::MissingUniqueConstraintField {
                table: "memberships".into(),
                constraint: "project_user".into(),
                field: missing.into(),
            }
        );
    }
    let mut declaration = membership();
    declaration.unique_constraints[0]
        .fields
        .push("projectId".into());
    assert_eq!(
        declaration.normalize().unwrap_err(),
        SchemaError::DuplicateUniqueConstraintField {
            table: "memberships".into(),
            constraint: "project_user".into(),
            field: "projectId".into(),
        }
    );
}

#[test]
fn reordered_duplicate_constraints_are_rejected_even_with_different_null_policies() {
    for reorder in [false, true] {
        let mut declaration = membership();
        declaration.fields[0].nullable = true;
        declaration.unique_constraints[0].null_policy = Some(UniqueNullPolicy::Distinct);
        let mut duplicate = declaration.unique_constraints[0].clone();
        duplicate.name = "duplicate".into();
        duplicate.null_policy = Some(UniqueNullPolicy::NotDistinct);
        if reorder {
            duplicate.fields.reverse();
        }
        declaration.unique_constraints.push(duplicate);
        assert_eq!(
            declaration.normalize().unwrap_err(),
            SchemaError::DuplicateUniqueConstraint {
                table: "memberships".into(),
                constraint: "duplicate".into(),
                existing_constraint: "project_user".into(),
            }
        );
    }
}

#[test]
fn constraints_resolve_injected_and_selected_keys_without_injecting_other_fields() {
    let mut declaration = membership();
    declaration.unique_constraints = vec![unique("id_constraint", &["id"])];
    assert_eq!(
        declaration.normalize().unwrap().unique_constraints()[0].fields(),
        &["id"]
    );

    let mut declaration = membership();
    declaration.primary_key = Some("projectId".into());
    let contract = declaration.clone().normalize().unwrap();
    assert!(contract.field("id").is_none());
    assert_eq!(
        contract.unique_constraints()[0].fields(),
        &["projectId", "userId"]
    );
    declaration
        .unique_constraints
        .push(unique("id_constraint", &["id"]));
    assert_eq!(
        declaration.normalize().unwrap_err(),
        SchemaError::MissingUniqueConstraintField {
            table: "memberships".into(),
            constraint: "id_constraint".into(),
            field: "id".into(),
        }
    );
}

#[test]
fn schema_keeps_table_scoped_names_uuid_defaults_and_primary_key_references() {
    let mut users = TableDeclaration::new(
        "users",
        vec![FieldDefinition::new("email", ScalarType::String)],
    );
    users
        .unique_constraints
        .push(unique("project_user", &["email"]));
    let mut schema = SchemaDeclaration::new(vec![
        membership(),
        users,
        TableDeclaration::new("projects", vec![]),
    ]);
    schema.default_uuid_generation = UuidGeneration::V4;
    schema.references = vec![
        ReferenceDeclaration::new("memberships", "userId", "users"),
        ReferenceDeclaration::new("memberships", "projectId", "projects"),
    ];
    let contract = schema.normalize().unwrap();
    assert_eq!(
        contract.table("memberships").unwrap().unique_constraints()[0].name(),
        "project_user"
    );
    assert_eq!(
        contract.table("users").unwrap().unique_constraints()[0].name(),
        "project_user"
    );
    assert_eq!(
        contract.table("memberships").unwrap().key_generation(),
        &KeyGeneration::UuidV4
    );
    assert!(
        contract
            .references()
            .iter()
            .all(|reference| reference.target_field() == "id")
    );

    let mut invalid = membership();
    invalid.unique_constraints[0].fields.push("missing".into());
    assert!(matches!(
        SchemaDeclaration::new(vec![invalid]).normalize(),
        Err(SchemaError::MissingUniqueConstraintField { .. })
    ));
}

#[test]
fn unique_fields_keep_default_and_partial_update_rules() {
    let mut label = FieldDefinition::new("label", ScalarType::String);
    label.nullable = true;
    label.default = Some(Value::Null);
    let mut declaration = TableDeclaration::new("labels", vec![label]);
    let mut constraint = unique("unique_label", &["label"]);
    constraint.null_policy = Some(UniqueNullPolicy::NotDistinct);
    declaration.unique_constraints.push(constraint);
    let contract = declaration.normalize().unwrap();
    let prepared = contract.prepare_insert(RecordValues::new()).unwrap();
    assert_eq!(prepared.key_action(), InsertKeyAction::Generate);
    assert_eq!(prepared.values().get("label"), Some(&Value::Null));
    let mut row = prepared.into_values();
    row.insert("id".into(), Value::Uuid([1; 16]));
    contract
        .apply_update(
            &mut row,
            RecordValues::from([("label".into(), Value::String("new".into()))]),
        )
        .unwrap();
    assert_eq!(row.get("label"), Some(&Value::String("new".into())));
    contract
        .apply_update(&mut row, RecordValues::new())
        .unwrap();
    assert_eq!(row.get("label"), Some(&Value::String("new".into())));
    assert_eq!(
        contract.unique_constraints()[0].null_policy(),
        UniqueNullPolicy::NotDistinct
    );
}
