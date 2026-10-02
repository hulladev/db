use hulladb_schema::{
    FieldDefinition, InsertKeyAction, RecordValues, ScalarType, TableDeclaration,
    ValidationErrorKind, Value,
};
use hulladb_wire::{WireError, WireErrorKind, decode_record, encode_record};

fn assert_field_error(input: &str, field: &str, kind: WireErrorKind) {
    let error = decode_record(input).unwrap_err();
    assert_eq!(
        error,
        WireError::Field {
            field: field.into(),
            kind
        }
    );
    assert!(error.to_string().contains(field));
}

#[test]
fn records_have_an_exact_deterministic_encoding_and_preserve_every_scalar() {
    let values = RecordValues::from([
        (
            "uuid".into(),
            Value::Uuid([
                0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd,
                0xee, 0xff,
            ]),
        ),
        ("string".into(), Value::String("9223372036854775807".into())),
        ("min".into(), Value::Int64(i64::MIN)),
        ("max".into(), Value::Int64(i64::MAX)),
        ("null".into(), Value::Null),
        ("true".into(), Value::Boolean(true)),
        ("false".into(), Value::Boolean(false)),
    ]);
    let encoded = encode_record(&values).unwrap();
    assert_eq!(
        encoded,
        r#"{"false":false,"max":{"$int64":"9223372036854775807"},"min":{"$int64":"-9223372036854775808"},"null":null,"string":"9223372036854775807","true":true,"uuid":{"$uuid":"00112233-4455-6677-8899-aabbccddeeff"}}"#
    );
    assert_eq!(decode_record(&encoded).unwrap(), values);
    assert_eq!(encode_record(&decode_record("{}").unwrap()).unwrap(), "{}");
}

#[test]
fn integers_beyond_javascript_number_precision_round_trip_exactly() {
    for value in [
        i64::MIN,
        i64::MIN + 1,
        -9_007_199_254_740_993,
        -1,
        0,
        1,
        9_007_199_254_740_991,
        9_007_199_254_740_992,
        9_007_199_254_740_993,
        i64::MAX - 1,
        i64::MAX,
    ] {
        let input = format!(r#"{{"value":{{"$int64":"{value}"}}}}"#);
        let decoded = decode_record(&input).unwrap();
        assert_eq!(decoded.get("value"), Some(&Value::Int64(value)));
        assert_eq!(encode_record(&decoded).unwrap(), input);
    }
}

#[test]
fn noncanonical_and_out_of_range_int64_text_is_rejected() {
    for text in [
        "",
        "+1",
        "-0",
        "00",
        "01",
        "-01",
        " 1",
        "1 ",
        "1.0",
        "1e3",
        "NaN",
        "Infinity",
        "9223372036854775808",
        "-9223372036854775809",
        "99999999999999999999999999999999999999",
        "١",
    ] {
        let input = format!(r#"{{"value":{{"$int64":"{text}"}}}}"#);
        assert_field_error(&input, "value", WireErrorKind::InvalidInt64);
    }
}

#[test]
fn uuid_hex_case_is_accepted_and_encoding_preserves_all_bytes() {
    let input = r#"{"id":{"$uuid":"00112233-4455-6677-8899-AABBCCDDEEFF"}}"#;
    let decoded = decode_record(input).unwrap();
    assert_eq!(
        decoded.get("id"),
        Some(&Value::Uuid([
            0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd,
            0xee, 0xff
        ]))
    );
    assert_eq!(encode_record(&decoded).unwrap(), input.to_lowercase());
    // Codec storage imposes neither a UUID version nor variant restriction.
    for bytes in [[0; 16], [255; 16], [0x44; 16], [0x77; 16]] {
        let values = RecordValues::from([("id".into(), Value::Uuid(bytes))]);
        assert_eq!(
            decode_record(&encode_record(&values).unwrap()).unwrap(),
            values
        );
    }
}

#[test]
fn malformed_uuid_forms_are_rejected_without_guessing() {
    for text in [
        "",
        "00112233445566778899aabbccddeeff",
        "{00112233-4455-6677-8899-aabbccddeeff}",
        "urn:uuid:00112233-4455-6677-8899-aabbccddeeff",
        "00112233_4455-6677-8899-aabbccddeeff",
        "00112233-4455-6677-8899-aabbccddeefg",
        " 00112233-4455-6677-8899-aabbccddeeff",
        "00112233-4455-6677-8899-aabbccddeeff ",
        "00112233-4455-6677-8899-aabbccddeef",
        "00112233-4455-6677-8899-aabbccddeé",
    ] {
        let input = format!(r#"{{"id":{{"$uuid":"{text}"}}}}"#);
        assert_field_error(&input, "id", WireErrorKind::InvalidUuid);
    }
}

#[test]
fn duplicate_fields_are_rejected_including_decoded_escape_aliases() {
    for input in [
        r#"{"title":"first","title":"second"}"#,
        r#"{"title":"same","title":"same"}"#,
        r#"{"title":null,"title":null}"#,
        r#"{"title":"first","ti\u0074le":"second"}"#,
        r#"{"ti\u0074le":"first","title":"second"}"#,
    ] {
        assert_field_error(input, "title", WireErrorKind::DuplicateField);
    }
    assert_field_error(
        r#"{"unknown":false,"unknown":true}"#,
        "unknown",
        WireErrorKind::DuplicateField,
    );
}

#[test]
fn scalar_tags_must_be_exactly_one_known_member_with_a_string_payload() {
    for raw in [
        r#"{}"#,
        r#"{"$int64":1}"#,
        r#"{"$int64":null}"#,
        r#"{"$uuid":true}"#,
        r#"{"$int64":[]}"#,
        r#"{"$int64":{"$int64":"1"}}"#,
        r#"{"$unknown":"1"}"#,
        r#"{"$Int64":"1"}"#,
        r#"{"$int64":"1","extra":true}"#,
        r#"{"$int64":"1","$int64":"2"}"#,
        r#"{"$int64":"1","$int\u00364":"2"}"#,
        r#"{"$int64":"1","$uuid":"00000000-0000-0000-0000-000000000000"}"#,
        r#"{"$uuid":"00000000-0000-0000-0000-000000000000","$uuid":"00000000-0000-0000-0000-000000000000"}"#,
    ] {
        assert_field_error(
            &format!(r#"{{"value":{raw}}}"#),
            "value",
            WireErrorKind::InvalidScalarTag,
        );
    }
}

#[test]
fn numbers_and_arrays_never_become_logical_scalars() {
    for raw in [
        "0",
        "-1",
        "1.0",
        "1e3",
        "9007199254740993",
        "9223372036854775807",
        "1e1000",
        "[]",
        "[true]",
        "[[[null]]]",
    ] {
        assert_field_error(
            &format!(r#"{{"value":{raw}}}"#),
            "value",
            WireErrorKind::UnsupportedValue,
        );
    }
}

#[test]
fn invalid_documents_and_trailing_content_are_rejected() {
    for input in [
        "",
        "null",
        "false",
        "1",
        "\"text\"",
        "[]",
        "{",
        "{\"x\":}",
        "{\"x\":true,}",
        "{} {}",
        "{}garbage",
        "{\"x\":NaN}",
        "{\"x\":\"\\ud800\"}",
    ] {
        assert!(decode_record(input).is_err(), "accepted {input:?}");
    }
    let WireError::Json { line, column, .. } = decode_record("{\n\"x\":\n}").unwrap_err() else {
        panic!("expected a JSON diagnostic");
    };
    assert!(line > 0 && column > 0);
    assert_eq!(
        decode_record(" \n {\"x\": null} \t ").unwrap().get("x"),
        Some(&Value::Null)
    );
}

#[test]
fn strings_and_field_names_round_trip_without_type_or_unicode_normalization() {
    let mut values = RecordValues::new();
    for text in [
        "",
        " 001 ",
        "00112233-4455-6677-8899-aabbccddeeff",
        "{\"$int64\":\"1\"}",
        "\0\n\t\"\\",
        "é",
        "e\u{301}",
        "🦀",
    ] {
        values.insert(text.into(), Value::String(text.into()));
    }
    assert_eq!(
        decode_record(&encode_record(&values).unwrap()).unwrap(),
        values
    );
    assert_eq!(
        decode_record(r#"{"$int64":"1"}"#).unwrap().get("$int64"),
        Some(&Value::String("1".into()))
    );
}

#[test]
fn decoding_and_schema_validation_remain_separate_with_no_coercion() {
    let mut note = FieldDefinition::new("note", ScalarType::String);
    note.nullable = true;
    note.default = Some(Value::Null);
    let table = TableDeclaration::new("items", vec![note])
        .normalize()
        .unwrap();
    let empty = decode_record("{}").unwrap();
    assert!(empty.is_empty());
    let prepared = table.prepare_insert(empty).unwrap();
    assert_eq!(prepared.key_action(), InsertKeyAction::Generate);
    assert_eq!(prepared.values().get("note"), Some(&Value::Null));
    assert!(!prepared.values().contains_key("id"));
    assert!(table.validate_row(prepared.values()).is_err());

    let unknown = decode_record(r#"{"unknown":true}"#).unwrap();
    assert_eq!(
        table.validate_update(&unknown).unwrap_err().kind,
        ValidationErrorKind::UnknownField
    );
    let plain_uuid =
        decode_record(r#"{"id":"00112233-4455-6677-8899-aabbccddeeff","note":null}"#).unwrap();
    assert_eq!(
        table.validate_row(&plain_uuid).unwrap_err().kind,
        ValidationErrorKind::TypeMismatch {
            expected: ScalarType::Uuid,
            actual: ScalarType::String
        }
    );

    let mut row =
        decode_record(r#"{"id":{"$uuid":"00112233-4455-6677-8899-aabbccddeeff"},"note":"old"}"#)
            .unwrap();
    table
        .apply_update(&mut row, decode_record("{}").unwrap())
        .unwrap();
    assert_eq!(row.get("note"), Some(&Value::String("old".into())));
    table
        .apply_update(&mut row, decode_record(r#"{"note":null}"#).unwrap())
        .unwrap();
    assert_eq!(row.get("note"), Some(&Value::Null));
    let original = row.clone();
    let patch =
        decode_record(r#"{"id":{"$uuid":"00112233-4455-6677-8899-aabbccddeeff"}}"#).unwrap();
    assert_eq!(
        table.apply_update(&mut row, patch).unwrap_err().kind,
        ValidationErrorKind::PrimaryKeyImmutable
    );
    assert_eq!(row, original);
}
