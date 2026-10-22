use hulladb_schema::{
    CheckDefinition, CheckOperand, FieldConstraints, FieldDefinition, IndexDeclaration,
    ReferenceDeclaration, ReferenceOnDelete, ScalarType, SchemaDeclaration, TableDeclaration,
    Value,
};

#[test]
fn bounds_choices_defaults_and_reference_policies_are_validated_before_install() {
    let mut text = FieldDefinition::new("name", ScalarType::String);
    text.constraints = FieldConstraints {
        min: Some(Value::Int64(2)),
        max: Some(Value::Int64(3)),
        one_of: vec![Value::String("🦀é".into())],
    };
    text.validate_value(&Value::String("🦀é".into())).unwrap();
    assert!(text.validate_value(&Value::String("🦀".into())).is_err());
    assert!(text.validate_value(&Value::String("ab".into())).is_err());
    text.default = Some(Value::String("ab".into()));
    assert!(
        TableDeclaration::new("items", vec![text.clone()])
            .normalize()
            .is_err()
    );
    text.default = None;
    text.constraints.min = Some(Value::Int64(4));
    assert!(
        TableDeclaration::new("items", vec![text])
            .normalize()
            .is_err()
    );
    let mut score = FieldDefinition::new("score", ScalarType::Int64);
    score.constraints.min = Some(Value::Int64(0));
    score.constraints.one_of = vec![Value::Int64(-1)];
    assert!(
        TableDeclaration::new("items", vec![score])
            .normalize()
            .is_err()
    );
    let mut numeric = FieldDefinition::new("rate", ScalarType::Float64);
    // JSON descriptors parse whole-valued floats as JSON integers, while driver
    // reads construct floats; membership must compare their numeric values.
    numeric.constraints.one_of = vec![Value::Float64(serde_json::Number::from(1))];
    numeric
        .validate_value(&Value::float64(1.0).unwrap())
        .unwrap();
    numeric
        .constraints
        .one_of
        .push(Value::float64(1.0).unwrap());
    assert!(
        TableDeclaration::new("rates", vec![numeric])
            .normalize()
            .is_err()
    );
    let mut schema = SchemaDeclaration::new(vec![
        TableDeclaration::new("parents", vec![]),
        TableDeclaration::new(
            "children",
            vec![FieldDefinition::new("parent", ScalarType::Uuid)],
        ),
    ]);
    let mut reference = ReferenceDeclaration::new("children", "parent", "parents");
    reference.on_delete = ReferenceOnDelete::SetNull;
    schema.references.push(reference);
    assert!(schema.clone().normalize().is_err());
    schema.tables[1].fields[0].nullable = true;
    schema.normalize().unwrap();
}

#[test]
fn invalid_indexes_and_check_templates_cannot_reach_sql_installation() {
    let mut table = TableDeclaration::new(
        "items",
        vec![FieldDefinition::new("quantity", ScalarType::Int64)],
    );
    table.indexes.push(IndexDeclaration {
        name: "quantity".into(),
        fields: vec!["typo".into()],
    });
    assert!(table.clone().normalize().is_err());
    table.indexes.clear();
    table.checks.push(CheckDefinition {
        name: "positive".into(),
        parts: vec!["".into(), "> 0".into()],
        operands: vec![CheckOperand::Field("quantity".into())],
    });
    table.clone().normalize().unwrap();
    table.checks[0].parts[1] = " > 0); DROP TABLE items; --".into();
    assert!(table.normalize().is_err());
}

#[test]
fn timestamp_choices_compare_stored_values_instead_of_fraction_spelling() {
    for scalar in [ScalarType::Timestamp, ScalarType::LocalTimestamp] {
        let value = |fraction: &str| {
            let suffix = if scalar == ScalarType::Timestamp {
                "Z"
            } else {
                ""
            };
            let text = format!("2026-10-20T12:00:00{fraction}{suffix}");
            if scalar == ScalarType::Timestamp {
                Value::Timestamp(text)
            } else {
                Value::LocalTimestamp(text)
            }
        };
        let mut field = FieldDefinition::new("created", scalar);
        field.constraints.one_of = vec![value(".100000000")];
        // Driver reads trim trailing fractional zeroes; an accepted stored
        // instant must remain valid when it is reused in a later write.
        field.validate_value(&value(".1")).unwrap();
        assert!(field.validate_value(&value(".101")).is_err());
        field.constraints.one_of.push(value(".1"));
        assert!(
            TableDeclaration::new("events", vec![field])
                .normalize()
                .is_err()
        );
    }
}
