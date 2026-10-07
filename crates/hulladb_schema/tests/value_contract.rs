use hulladb_schema::{
    FieldDefinition, InsertKeyAction, InsertKeyPolicy, KeyGeneration, RecordValues, ScalarType,
    TableContract, TableDeclaration, ValidationError, ValidationErrorKind, Value,
};

fn tasks() -> TableContract {
    let mut notes = FieldDefinition::new("notes", ScalarType::String);
    notes.nullable = true;
    TableDeclaration::new(
        "tasks",
        vec![FieldDefinition::new("title", ScalarType::String), notes],
    )
    .normalize()
    .unwrap()
}

fn input() -> RecordValues {
    RecordValues::from([
        ("title".into(), Value::String("Build validator".into())),
        ("notes".into(), Value::Null),
    ])
}

fn error(field: &str, kind: ValidationErrorKind) -> ValidationError {
    ValidationError {
        table: "tasks".into(),
        field: field.into(),
        kind,
    }
}

#[test]
fn nullable_fields_require_presence_but_accept_explicit_null() {
    let table = tasks();
    let mut values = input();
    assert_eq!(
        table.validate_insert(&values),
        Ok(InsertKeyAction::Generate)
    );

    values.insert("id".into(), Value::Uuid([1; 16]));
    assert_eq!(table.validate_row(&values), Ok(()));
    values.remove("notes");
    let expected = error("notes", ValidationErrorKind::MissingField);
    assert_eq!(table.validate_insert(&values), Err(expected.clone()));
    assert_eq!(table.validate_row(&values), Err(expected.clone()));
    assert!(
        expected
            .to_string()
            .contains("nullability does not permit omission")
    );

    values.insert("notes".into(), Value::String("Review this chunk".into()));
    assert_eq!(table.validate_row(&values), Ok(()));
}

#[test]
fn non_nullable_fields_reject_both_omission_and_null() {
    let table = tasks();
    let mut values = input();
    values.insert("id".into(), Value::Uuid([1; 16]));
    for (value, kind) in [
        (None, ValidationErrorKind::MissingField),
        (Some(Value::Null), ValidationErrorKind::NullNotAllowed),
    ] {
        values.remove("title");
        if let Some(value) = value {
            values.insert("title".into(), value);
        }
        assert_eq!(
            table.validate_insert(&values),
            Err(error("title", kind.clone()))
        );
        assert_eq!(table.validate_row(&values), Err(error("title", kind)));
    }
}

#[test]
fn unknown_fields_are_rejected_instead_of_discarded() {
    let table = tasks();
    let mut values = input();
    values.insert("id".into(), Value::Uuid([1; 16]));
    values.insert("note".into(), Value::String("Typo".into()));
    let expected = error("note", ValidationErrorKind::UnknownField);
    assert_eq!(table.validate_insert(&values), Err(expected.clone()));
    assert_eq!(table.validate_row(&values), Err(expected));
}

#[test]
fn an_extra_field_cannot_take_the_place_of_an_omitted_generated_key() {
    let table = tasks();
    let mut values = input();
    values.insert("note".into(), Value::Null);
    // The input has as many entries as the complete schema, but id is omitted.
    assert_eq!(values.len(), table.fields().len());
    assert_eq!(
        table.validate_insert(&values),
        Err(error("note", ValidationErrorKind::UnknownField))
    );
}

#[test]
fn matching_field_counts_cannot_hide_a_missing_declared_field() {
    let table = tasks();
    let mut values = input();
    values.insert("id".into(), Value::Uuid([1; 16]));
    values.remove("notes");
    values.insert("note".into(), Value::Null);
    assert_eq!(values.len(), table.fields().len());
    let expected = error("notes", ValidationErrorKind::MissingField);
    assert_eq!(table.validate_row(&values), Err(expected.clone()));
    assert_eq!(table.validate_insert(&values), Err(expected));
}

#[test]
fn scalar_values_must_match_without_coercion_even_for_nullable_fields() {
    let samples = [
        (ScalarType::String, Value::String("42".into())),
        (ScalarType::Boolean, Value::Boolean(true)),
        (ScalarType::Int64, Value::Int64(42)),
        (ScalarType::Uuid, Value::Uuid([1; 16])),
    ];
    for (expected, _) in &samples {
        for nullable in [false, true] {
            let mut field = FieldDefinition::new("value", *expected);
            field.nullable = nullable;
            let table = TableDeclaration::new("tasks", vec![field])
                .normalize()
                .unwrap();
            for (actual, value) in &samples {
                let values = RecordValues::from([
                    ("id".into(), Value::Uuid([1; 16])),
                    ("value".into(), value.clone()),
                ]);
                if expected == actual {
                    assert_eq!(table.validate_row(&values), Ok(()));
                    assert_eq!(
                        table.validate_insert(&values),
                        Ok(InsertKeyAction::Provided)
                    );
                } else {
                    let diagnostic = error(
                        "value",
                        ValidationErrorKind::TypeMismatch {
                            expected: *expected,
                            actual: *actual,
                        },
                    );
                    assert_eq!(table.validate_row(&values), Err(diagnostic.clone()));
                    assert_eq!(table.validate_insert(&values), Err(diagnostic));
                }
            }
            let values = RecordValues::from([
                ("id".into(), Value::Uuid([1; 16])),
                ("value".into(), Value::Null),
            ]);
            assert_eq!(table.validate_row(&values).is_ok(), nullable);
            assert_eq!(table.validate_insert(&values).is_ok(), nullable);
        }
    }
}

#[test]
fn insert_key_policy_matrix_and_complete_row_requirements() {
    for generator in [
        KeyGeneration::None,
        KeyGeneration::UuidV4,
        KeyGeneration::UuidV7,
        KeyGeneration::Custom {
            handler: "tasks.new_key".into(),
            output_type: ScalarType::Uuid,
        },
    ] {
        for policy in [
            InsertKeyPolicy::AllowExplicit,
            InsertKeyPolicy::GeneratedOnly,
        ] {
            if generator == KeyGeneration::None && policy == InsertKeyPolicy::GeneratedOnly {
                continue; // Already rejected by schema normalization.
            }
            let mut declaration = TableDeclaration::new("tasks", vec![]);
            declaration.key_generation = Some(generator.clone());
            declaration.insert_key_policy = policy;
            let table = declaration.normalize().unwrap();
            let mut values = RecordValues::new();
            let missing = error("id", ValidationErrorKind::MissingField);
            let expected = if generator == KeyGeneration::None {
                Err(missing.clone())
            } else {
                Ok(InsertKeyAction::Generate)
            };
            assert_eq!(table.validate_insert(&values), expected);
            assert_eq!(table.validate_row(&values), Err(missing));

            values.insert("id".into(), Value::Uuid([1; 16]));
            let expected = if policy == InsertKeyPolicy::GeneratedOnly {
                Err(error("id", ValidationErrorKind::ExplicitKeyForbidden))
            } else {
                Ok(InsertKeyAction::Provided)
            };
            assert_eq!(table.validate_insert(&values), expected);
            // A generated-only key is still present in the final stored row.
            assert_eq!(table.validate_row(&values), Ok(()));

            values.insert("id".into(), Value::Null);
            let kind = if policy == InsertKeyPolicy::GeneratedOnly {
                ValidationErrorKind::ExplicitKeyForbidden
            } else {
                ValidationErrorKind::NullNotAllowed
            };
            assert_eq!(table.validate_insert(&values), Err(error("id", kind)));
            assert_eq!(
                table.validate_row(&values),
                Err(error("id", ValidationErrorKind::NullNotAllowed))
            );
        }
    }
}

#[test]
fn generation_uses_the_selected_key_and_its_output_must_validate() {
    let mut declaration = TableDeclaration::new(
        "tasks",
        vec![
            FieldDefinition::new("slug", ScalarType::String),
            FieldDefinition::new("id", ScalarType::Int64),
        ],
    );
    declaration.primary_key = Some("slug".into());
    declaration.key_generation = Some(KeyGeneration::Custom {
        handler: "tasks.slug".into(),
        output_type: ScalarType::String,
    });
    let table = declaration.normalize().unwrap();
    let mut values = RecordValues::new();
    // Ordinary fields called id do not receive special omission behavior.
    assert_eq!(
        table.validate_insert(&values),
        Err(error("id", ValidationErrorKind::MissingField))
    );
    values.insert("id".into(), Value::Int64(1));
    assert_eq!(
        table.validate_insert(&values),
        Ok(InsertKeyAction::Generate)
    );
    values.insert("slug".into(), Value::Boolean(true));
    let diagnostic = error(
        "slug",
        ValidationErrorKind::TypeMismatch {
            expected: ScalarType::String,
            actual: ScalarType::Boolean,
        },
    );
    assert_eq!(table.validate_row(&values), Err(diagnostic.clone()));
    assert_eq!(table.validate_insert(&values), Err(diagnostic));
    values.insert("slug".into(), Value::String("build-validator".into()));
    assert_eq!(table.validate_row(&values), Ok(()));
}
