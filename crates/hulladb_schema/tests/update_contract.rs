use hulladb_schema::{
    FieldDefinition, InsertKeyPolicy, RecordValues, ScalarType, TableContract, TableDeclaration,
    ValidationErrorKind, Value,
};

fn tasks() -> TableContract {
    let mut notes = FieldDefinition::new("notes", ScalarType::String);
    notes.nullable = true;
    notes.default = Some(Value::String("Default notes".into()));
    let mut done = FieldDefinition::new("done", ScalarType::Boolean);
    done.default = Some(Value::Boolean(false));
    let mut declaration = TableDeclaration::new(
        "tasks",
        vec![
            FieldDefinition::new("title", ScalarType::String),
            notes,
            done,
        ],
    );
    declaration.insert_key_policy = InsertKeyPolicy::GeneratedOnly;
    declaration.normalize().unwrap()
}

fn row() -> RecordValues {
    RecordValues::from([
        ("id".into(), Value::Uuid([1; 16])),
        ("title".into(), Value::String("Original title".into())),
        ("notes".into(), Value::String("Existing notes".into())),
        ("done".into(), Value::Boolean(true)),
    ])
}

#[test]
fn empty_updates_are_valid_and_leave_the_row_unchanged() {
    let table = tasks();
    let mut values = row();
    let original = values.clone();
    assert_eq!(table.validate_update(&RecordValues::new()), Ok(()));
    assert_eq!(table.apply_update(&mut values, RecordValues::new()), Ok(()));
    assert_eq!(values, original);
}

#[test]
fn omission_preserves_values_and_does_not_apply_defaults_or_generate_keys() {
    let table = tasks();
    let mut values = row();
    let changes = RecordValues::from([("title".into(), Value::String("New title".into()))]);
    let original_changes = changes.clone();
    assert_eq!(table.validate_update(&changes), Ok(()));
    assert_eq!(changes, original_changes);
    assert_eq!(table.apply_update(&mut values, changes), Ok(()));
    assert_eq!(
        values.get("title"),
        Some(&Value::String("New title".into()))
    );
    assert_eq!(
        values.get("notes"),
        Some(&Value::String("Existing notes".into()))
    );
    assert_eq!(values.get("done"), Some(&Value::Boolean(true)));
    assert_eq!(values.get("id"), Some(&Value::Uuid([1; 16])));
    assert_eq!(table.validate_row(&values), Ok(()));
}

#[test]
fn explicit_null_clears_a_nullable_field_without_using_its_default() {
    let table = tasks();
    let mut values = row();
    let changes = RecordValues::from([("notes".into(), Value::Null)]);
    assert_eq!(table.apply_update(&mut values, changes), Ok(()));
    assert_eq!(values.get("notes"), Some(&Value::Null));
    assert_eq!(table.validate_row(&values), Ok(()));

    let changes = RecordValues::from([("notes".into(), Value::String("Restored".into()))]);
    assert_eq!(table.apply_update(&mut values, changes), Ok(()));
    assert_eq!(values.get("notes"), Some(&Value::String("Restored".into())));
}

#[test]
fn invalid_updates_do_not_partially_modify_the_row() {
    let table = tasks();
    for (name, value, kind) in [
        ("title", Value::Null, ValidationErrorKind::NullNotAllowed),
        (
            "notes",
            Value::Int64(42),
            ValidationErrorKind::TypeMismatch {
                expected: ScalarType::String,
                actual: ScalarType::Int64,
            },
        ),
        ("unknown", Value::Null, ValidationErrorKind::UnknownField),
    ] {
        let mut values = row();
        let original = values.clone();
        let changes = RecordValues::from([
            ("done".into(), Value::Boolean(false)), // Valid value checked first.
            (name.into(), value),
        ]);
        let error = table.validate_update(&changes).unwrap_err();
        assert_eq!(error.table, "tasks");
        assert_eq!(error.field, name);
        assert_eq!(error.kind, kind);
        assert_eq!(table.apply_update(&mut values, changes), Err(error));
        assert_eq!(values, original);
    }
}

#[test]
fn primary_keys_cannot_be_supplied_even_with_the_same_value() {
    let table = tasks();
    for key in [Value::Uuid([1; 16]), Value::Uuid([2; 16]), Value::Null] {
        let mut values = row();
        let original = values.clone();
        let changes =
            RecordValues::from([("done".into(), Value::Boolean(false)), ("id".into(), key)]);
        let error = table.validate_update(&changes).unwrap_err();
        assert_eq!(error.field, "id");
        assert_eq!(error.kind, ValidationErrorKind::PrimaryKeyImmutable);
        assert!(error.to_string().contains("primary key cannot be supplied"));
        assert_eq!(table.apply_update(&mut values, changes), Err(error));
        assert_eq!(values, original);
    }
}

#[test]
fn immutability_follows_the_selected_key_rather_than_the_name_id() {
    let mut declaration = TableDeclaration::new(
        "tasks",
        vec![
            FieldDefinition::new("slug", ScalarType::String),
            FieldDefinition::new("id", ScalarType::Int64),
        ],
    );
    declaration.primary_key = Some("slug".into());
    let table = declaration.normalize().unwrap();
    let mut values = RecordValues::from([
        ("slug".into(), Value::String("task-1".into())),
        ("id".into(), Value::Int64(1)),
    ]);
    assert_eq!(
        table.apply_update(
            &mut values,
            RecordValues::from([("id".into(), Value::Int64(2)),])
        ),
        Ok(())
    );
    assert_eq!(values.get("id"), Some(&Value::Int64(2)));
    let original = values.clone();
    let error = table
        .apply_update(
            &mut values,
            RecordValues::from([("slug".into(), Value::String("task-2".into()))]),
        )
        .unwrap_err();
    assert_eq!(error.field, "slug");
    assert_eq!(error.kind, ValidationErrorKind::PrimaryKeyImmutable);
    assert_eq!(values, original);
}

#[test]
fn an_invalid_base_row_is_rejected_without_being_repaired_or_modified() {
    let table = tasks();
    let mut values = row();
    values.remove("notes"); // Its insert default must not repair a stored row.
    let original = values.clone();
    let error = table
        .apply_update(
            &mut values,
            RecordValues::from([("title".into(), Value::String("New title".into()))]),
        )
        .unwrap_err();
    assert_eq!(error.field, "notes");
    assert_eq!(error.kind, ValidationErrorKind::MissingField);
    assert_eq!(values, original);
}

#[test]
fn updates_validate_all_scalar_types_without_coercion() {
    let samples = [
        (ScalarType::String, Value::String("42".into())),
        (ScalarType::Boolean, Value::Boolean(true)),
        (ScalarType::Int64, Value::Int64(i64::MAX)),
        (ScalarType::Uuid, Value::Uuid([2; 16])),
    ];
    for (expected, _) in &samples {
        let table = TableDeclaration::new("tasks", vec![FieldDefinition::new("value", *expected)])
            .normalize()
            .unwrap();
        for (actual, value) in &samples {
            let changes = RecordValues::from([("value".into(), value.clone())]);
            if actual == expected {
                assert_eq!(table.validate_update(&changes), Ok(()));
            } else {
                assert_eq!(
                    table.validate_update(&changes).unwrap_err().kind,
                    ValidationErrorKind::TypeMismatch {
                        expected: *expected,
                        actual: *actual
                    }
                );
            }
        }
    }
}
