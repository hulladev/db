use hulladb_schema::{
    FieldDefinition, ProjectionErrorKind, RecordValues, ScalarType, TableDeclaration,
    ValidationErrorKind, Value,
};

fn table() -> hulladb_schema::TableContract {
    let mut notes = FieldDefinition::new("notes", ScalarType::String);
    notes.nullable = true;
    notes.default = Some(Value::Null);
    TableDeclaration::new(
        "tasks",
        vec![notes, FieldDefinition::new("secret", ScalarType::String)],
    )
    .normalize()
    .unwrap()
}

#[test]
fn projection_is_not_a_complete_row_and_never_inherits_defaults_or_extra_fields() {
    let table = table();
    let projection = table.projection("summary", &["id", "notes"]).unwrap();
    assert_eq!(projection.table(), "tasks");
    assert_eq!(projection.name(), "summary");
    assert_eq!(
        projection
            .fields()
            .iter()
            .map(|field| field.name.as_str())
            .collect::<Vec<_>>(),
        ["id", "notes"]
    );
    assert!(projection.field("notes").unwrap().default.is_none());
    assert!(projection.field("secret").is_none());
    let mut result = RecordValues::from([
        ("id".into(), Value::Uuid([1; 16])),
        ("notes".into(), Value::Null),
    ]);
    projection.validate_record(&result).unwrap();
    assert_eq!(table.validate_row(&result).unwrap_err().field, "secret");
    result.remove("notes");
    assert_eq!(
        projection.validate_record(&result).unwrap_err().kind,
        ValidationErrorKind::MissingField
    );
    result.insert("notes".into(), Value::Null);
    result.insert(
        "secret".into(),
        Value::String("must not be delivered".into()),
    );
    assert_eq!(
        projection.validate_record(&result).unwrap_err().kind,
        ValidationErrorKind::UnknownField
    );
}

#[test]
fn projection_preserves_scalar_and_null_validation() {
    let projection = table().projection("summary", &["notes", "id"]).unwrap();
    for (key, notes, field, kind) in [
        (
            Value::Null,
            Value::Null,
            "id",
            ValidationErrorKind::NullNotAllowed,
        ),
        (
            Value::Uuid([1; 16]),
            Value::Boolean(false),
            "notes",
            ValidationErrorKind::TypeMismatch {
                expected: ScalarType::String,
                actual: ScalarType::Boolean,
            },
        ),
    ] {
        let error = projection
            .validate_record(&RecordValues::from([
                ("id".into(), key),
                ("notes".into(), notes),
            ]))
            .unwrap_err();
        assert_eq!(error.field, field);
        assert_eq!(error.kind, kind);
    }
}

#[test]
fn malformed_entity_projections_are_rejected_and_custom_keys_are_required() {
    let table = table();
    for (name, fields, kind) in [
        ("", vec!["id"], ProjectionErrorKind::EmptyName),
        ("summary", vec![], ProjectionErrorKind::EmptySelection),
        (
            "summary",
            vec!["id", "missing"],
            ProjectionErrorKind::UnknownField("missing".into()),
        ),
        (
            "summary",
            vec!["id", "id"],
            ProjectionErrorKind::DuplicateField("id".into()),
        ),
        (
            "summary",
            vec!["notes"],
            ProjectionErrorKind::MissingPrimaryKey("id".into()),
        ),
    ] {
        assert_eq!(table.projection(name, &fields).unwrap_err().kind, kind);
    }
    let mut declaration = TableDeclaration::new(
        "pages",
        vec![FieldDefinition::new("slug", ScalarType::String)],
    );
    declaration.primary_key = Some("slug".into());
    let custom = declaration
        .normalize()
        .unwrap()
        .projection("identity", &["slug"])
        .unwrap();
    custom
        .validate_record(&RecordValues::from([(
            "slug".into(),
            Value::String("hello".into()),
        )]))
        .unwrap();
}
