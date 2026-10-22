use hulladb_schema::{FieldDefinition, ScalarType, TableDeclaration, Value};

#[test]
fn calendar_and_precision_validation_prevents_rounding_or_timezone_guessing() {
    let date = FieldDefinition::new("day", ScalarType::Date);
    assert!(
        date.validate_value(&Value::Date("2024-02-29".into()))
            .is_ok()
    );
    for bad in [
        "2023-02-29",
        "2024-04-31",
        "0000-01-01",
        "10000-01-01",
        "2024-1-01",
        "infinity",
    ] {
        assert!(
            date.validate_value(&Value::Date(bad.into())).is_err(),
            "{bad}"
        );
    }
    let mut timestamp = FieldDefinition::new("instant", ScalarType::Timestamp);
    assert!(
        timestamp
            .validate_value(&Value::Timestamp("2024-02-29T23:59:59.123Z".into()))
            .is_ok()
    );
    assert!(
        timestamp
            .validate_value(&Value::Timestamp("2024-02-29T23:59:59.123000Z".into()))
            .is_ok()
    );
    for bad in [
        "2024-02-29T23:59:59.1234Z",
        "2024-02-29T24:00:00Z",
        "2024-02-29T23:59:60Z",
        "2024-02-29T23:59:59+00:00",
        "2024-02-29T23:59:59",
        "2024-02-29T23:59:59.Z",
    ] {
        assert!(
            timestamp
                .validate_value(&Value::Timestamp(bad.into()))
                .is_err(),
            "{bad}"
        );
    }
    timestamp.precision = Some(6);
    assert!(
        timestamp
            .validate_value(&Value::Timestamp("2024-02-29T23:59:59.123456Z".into()))
            .is_ok()
    );
    let local = FieldDefinition::new("clock", ScalarType::LocalTimestamp);
    assert!(
        local
            .validate_value(&Value::LocalTimestamp("2024-10-27T02:30:00".into()))
            .is_ok()
    );
    assert!(
        local
            .validate_value(&Value::LocalTimestamp("2024-10-27T02:30:00Z".into()))
            .is_err()
    );
    timestamp.precision = Some(7);
    assert!(
        TableDeclaration::new("events", vec![timestamp])
            .normalize()
            .is_err()
    );
}
