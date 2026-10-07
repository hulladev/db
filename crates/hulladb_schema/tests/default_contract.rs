use hulladb_schema::{
    FieldDefinition, InsertKeyAction, InsertKeyPolicy, RecordValues, ScalarType, SchemaError,
    TableDeclaration, ValidationErrorKind, Value,
};

fn default_field(name: &str, scalar_type: ScalarType, value: Value) -> FieldDefinition {
    let mut field = FieldDefinition::new(name, scalar_type);
    field.default = Some(value);
    field
}

fn explicit_key() -> RecordValues {
    RecordValues::from([("id".into(), Value::Uuid([1; 16]))])
}

#[test]
fn normalization_accepts_matching_literals_and_explicit_nullable_null_defaults() {
    for (scalar_type, value) in [
        (ScalarType::String, Value::String("Untitled".into())),
        (ScalarType::Boolean, Value::Boolean(false)),
        (ScalarType::Int64, Value::Int64(i64::MAX)),
        (ScalarType::Uuid, Value::Uuid([1; 16])),
    ] {
        let field = default_field("value", scalar_type, value);
        let contract = TableDeclaration::new("tasks", vec![field.clone()])
            .normalize()
            .unwrap();
        assert_eq!(contract.field("value"), Some(&field));

        let mut nullable = default_field("value", scalar_type, Value::Null);
        nullable.nullable = true;
        assert!(
            TableDeclaration::new("tasks", vec![nullable])
                .normalize()
                .is_ok()
        );
    }
}

#[test]
fn invalid_defaults_are_schema_errors_even_if_every_insert_would_supply_a_value() {
    for (scalar_type, value, reason) in [
        (
            ScalarType::String,
            Value::Null,
            ValidationErrorKind::NullNotAllowed,
        ),
        (
            ScalarType::Boolean,
            Value::String("false".into()),
            ValidationErrorKind::TypeMismatch {
                expected: ScalarType::Boolean,
                actual: ScalarType::String,
            },
        ),
    ] {
        let declaration =
            TableDeclaration::new("tasks", vec![default_field("value", scalar_type, value)]);
        assert_eq!(
            declaration.normalize(),
            Err(SchemaError::InvalidFieldDefault {
                table: "tasks".into(),
                field: "value".into(),
                reason,
            })
        );
    }
    let mut field = default_field("notes", ScalarType::String, Value::Int64(1));
    field.nullable = true;
    assert!(matches!(
        TableDeclaration::new("tasks", vec![field]).normalize(),
        Err(SchemaError::InvalidFieldDefault {
            reason: ValidationErrorKind::TypeMismatch { .. },
            ..
        })
    ));
}

#[test]
fn defaults_cannot_bypass_the_selected_primary_keys_generation_policy() {
    for key in ["id", "slug"] {
        let mut declaration = TableDeclaration::new(
            "tasks",
            vec![default_field(
                key,
                ScalarType::String,
                Value::String("fixed-key".into()),
            )],
        );
        declaration.primary_key = Some(key.into());
        assert_eq!(
            declaration.normalize(),
            Err(SchemaError::DefaultOnPrimaryKey {
                table: "tasks".into(),
                field: key.into(),
            })
        );
    }
    // A field named id can have a default when it is not the selected key.
    let mut declaration = TableDeclaration::new(
        "tasks",
        vec![
            default_field("id", ScalarType::Int64, Value::Int64(0)),
            FieldDefinition::new("slug", ScalarType::String),
        ],
    );
    declaration.primary_key = Some("slug".into());
    let contract = declaration.normalize().unwrap();
    let prepared = contract
        .prepare_insert(RecordValues::from([(
            "slug".into(),
            Value::String("task-1".into()),
        )]))
        .unwrap();
    assert_eq!(prepared.values().get("id"), Some(&Value::Int64(0)));
    assert_eq!(prepared.key_action(), InsertKeyAction::Provided);
}

#[test]
fn preparation_fills_only_omitted_fields_and_preserves_supplied_values() {
    let mut notes = default_field(
        "notes",
        ScalarType::String,
        Value::String("Initial notes".into()),
    );
    notes.nullable = true;
    let contract = TableDeclaration::new(
        "tasks",
        vec![
            default_field(
                "title",
                ScalarType::String,
                Value::String("Untitled".into()),
            ),
            default_field("done", ScalarType::Boolean, Value::Boolean(false)),
            default_field("priority", ScalarType::Int64, Value::Int64(i64::MIN)),
            default_field("external_id", ScalarType::Uuid, Value::Uuid([2; 16])),
            notes,
        ],
    )
    .normalize()
    .unwrap();

    for supplied in [
        None,
        Some(Value::Null),
        Some(Value::String("My notes".into())),
    ] {
        let mut values = explicit_key();
        values.insert("done".into(), Value::Boolean(true));
        if let Some(value) = &supplied {
            values.insert("notes".into(), value.clone());
        }
        assert_eq!(
            contract.validate_insert(&values),
            Ok(InsertKeyAction::Provided)
        );
        let prepared = contract.prepare_insert(values).unwrap();
        assert_eq!(prepared.key_action(), InsertKeyAction::Provided);
        assert_eq!(prepared.values().get("done"), Some(&Value::Boolean(true)));
        assert_eq!(
            prepared.values().get("title"),
            Some(&Value::String("Untitled".into()))
        );
        assert_eq!(
            prepared.values().get("priority"),
            Some(&Value::Int64(i64::MIN))
        );
        assert_eq!(
            prepared.values().get("external_id"),
            Some(&Value::Uuid([2; 16]))
        );
        assert_eq!(
            prepared.values().get("notes"),
            Some(&supplied.unwrap_or(Value::String("Initial notes".into())))
        );
        assert_eq!(contract.validate_row(prepared.values()), Ok(()));
    }
}

#[test]
fn invalid_supplied_values_are_not_replaced_by_defaults() {
    let contract = TableDeclaration::new(
        "tasks",
        vec![default_field(
            "done",
            ScalarType::Boolean,
            Value::Boolean(false),
        )],
    )
    .normalize()
    .unwrap();
    for (value, reason) in [
        (Value::Null, ValidationErrorKind::NullNotAllowed),
        (
            Value::String("true".into()),
            ValidationErrorKind::TypeMismatch {
                expected: ScalarType::Boolean,
                actual: ScalarType::String,
            },
        ),
    ] {
        let mut values = explicit_key();
        values.insert("done".into(), value);
        let error = contract.validate_insert(&values).unwrap_err();
        assert_eq!(error.field, "done");
        assert_eq!(error.kind, reason);
        assert_eq!(contract.prepare_insert(values).unwrap_err(), error);
    }
}

#[test]
fn complete_row_validation_never_fills_or_assumes_defaults() {
    let mut notes = default_field("notes", ScalarType::String, Value::Null);
    notes.nullable = true;
    let contract = TableDeclaration::new("tasks", vec![notes])
        .normalize()
        .unwrap();
    let values = explicit_key();
    assert_eq!(
        contract.validate_insert(&values),
        Ok(InsertKeyAction::Provided)
    );
    let error = contract.validate_row(&values).unwrap_err();
    assert_eq!(error.field, "notes");
    assert_eq!(error.kind, ValidationErrorKind::MissingField);
    let prepared = contract.prepare_insert(values).unwrap();
    assert_eq!(prepared.values().get("notes"), Some(&Value::Null));
    assert_eq!(contract.validate_row(prepared.values()), Ok(()));
}

#[test]
fn omitted_defaults_and_generated_keys_cannot_hide_unknown_fields() {
    let contract = TableDeclaration::new(
        "tasks",
        vec![
            default_field(
                "title",
                ScalarType::String,
                Value::String("Untitled".into()),
            ),
            default_field("done", ScalarType::Boolean, Value::Boolean(false)),
        ],
    )
    .normalize()
    .unwrap();
    for supply_key in [false, true] {
        let mut values = if supply_key {
            explicit_key()
        } else {
            RecordValues::new()
        };
        values.insert("typo".into(), Value::Null);
        // Input size is smaller than schema size, yet it contains an unknown name.
        assert!(values.len() < contract.fields().len());
        let error = contract.validate_insert(&values).unwrap_err();
        assert_eq!(error.field, "typo");
        assert_eq!(error.kind, ValidationErrorKind::UnknownField);
        assert_eq!(contract.prepare_insert(values).unwrap_err(), error);
    }
}

#[test]
fn defaults_do_not_make_other_required_fields_optional() {
    let contract = TableDeclaration::new(
        "tasks",
        vec![
            default_field("done", ScalarType::Boolean, Value::Boolean(false)),
            FieldDefinition::new("title", ScalarType::String),
        ],
    )
    .normalize()
    .unwrap();
    let values = RecordValues::new();
    let error = contract.validate_insert(&values).unwrap_err();
    assert_eq!(error.field, "title");
    assert_eq!(error.kind, ValidationErrorKind::MissingField);
    assert_eq!(contract.prepare_insert(values).unwrap_err(), error);
}

#[test]
fn prepared_defaults_are_owned_and_generated_keys_are_still_pending() {
    let mut declaration = TableDeclaration::new(
        "tasks",
        vec![default_field(
            "title",
            ScalarType::String,
            Value::String("Untitled".into()),
        )],
    );
    declaration.insert_key_policy = InsertKeyPolicy::GeneratedOnly;
    let contract = declaration.normalize().unwrap();
    let prepared = contract.prepare_insert(RecordValues::new()).unwrap();
    assert_eq!(prepared.key_action(), InsertKeyAction::Generate);
    assert!(!prepared.values().contains_key("id"));
    assert_eq!(
        contract.validate_row(prepared.values()).unwrap_err().field,
        "id"
    );
    let mut values = prepared.into_values();
    if let Some(Value::String(title)) = values.get_mut("title") {
        title.push_str(" edited");
    }
    values.insert("id".into(), Value::Uuid([1; 16]));
    assert_eq!(contract.validate_row(&values), Ok(()));

    let next = contract.prepare_insert(RecordValues::new()).unwrap();
    assert_eq!(
        next.values().get("title"),
        Some(&Value::String("Untitled".into()))
    );
    assert_eq!(
        contract.field("title").unwrap().default,
        Some(Value::String("Untitled".into()))
    );
    assert_eq!(
        contract.prepare_insert(explicit_key()).unwrap_err().kind,
        ValidationErrorKind::ExplicitKeyForbidden
    );
}
