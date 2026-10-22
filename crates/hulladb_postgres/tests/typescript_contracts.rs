#![cfg(feature = "typescript")]

use std::sync::Arc;

use hulladb_postgres::managed::{
    FunctionContract,
    typescript::{TypescriptFunction, Worker},
};
use serde_json::{Value as Json, json};

#[test]
fn native_validation_agrees_with_browser_wire_contracts() {
    let cases: Vec<Json> = serde_json::from_str(include_str!(
        "../../../packages/db/tests/fixtures/wire-contracts.json"
    ))
    .unwrap();
    let worker = Arc::new(Worker::new("unused", vec![]));
    for case in cases {
        let manifest = json!({"generatedBy": "@hulla/db", "functions": {"test.query": {"kind": "query", "input": case["shape"], "result": case["shape"]}}});
        let function =
            TypescriptFunction::from_manifest(worker.clone(), "test.query", &manifest.to_string())
                .unwrap();
        for value in case["valid"].as_array().unwrap() {
            assert!(
                function.validate_input(value).is_ok(),
                "{}: {value}",
                case["name"]
            );
            assert!(
                function.validate_result(value).is_ok(),
                "{}: {value}",
                case["name"]
            );
        }
        for value in case["invalid"].as_array().unwrap() {
            assert!(
                function.validate_input(value).is_err(),
                "{}: {value}",
                case["name"]
            );
            assert!(
                function.validate_result(value).is_err(),
                "{}: {value}",
                case["name"]
            );
        }
    }
}

#[test]
fn effective_payload_limits_count_encoded_utf8_and_allow_trusted_overrides() {
    let worker = Arc::new(Worker::new("unused", vec![]));
    let manifest = json!({"generatedBy":"@hulla/db", "limits":{"inputBytes":4,"outputBytes":4}, "functions":{
        "test.base":{"kind":"query","input":{"kind":"string"},"result":{"kind":"string"}},
        "test.larger":{"kind":"query","input":{"kind":"string"},"result":{"kind":"string"},"limits":{"inputBytes":8,"outputBytes":8}}
    }});
    let base =
        TypescriptFunction::from_manifest(worker.clone(), "test.base", &manifest.to_string())
            .unwrap();
    assert!(base.validate_input(&json!("é")).is_ok());
    assert!(base.validate_input(&json!("éé")).is_err());
    assert!(base.validate_result(&json!("éé")).is_err());
    let larger =
        TypescriptFunction::from_manifest(worker.clone(), "test.larger", &manifest.to_string())
            .unwrap();
    assert!(larger.validate_input(&json!("éé")).is_ok());
    assert!(larger.validate_result(&json!("éé")).is_ok());
    let mut invalid = manifest;
    invalid["functions"]["test.base"]["limits"] = json!({"executionMs":0});
    assert!(
        TypescriptFunction::from_manifest(worker.clone(), "test.base", &invalid.to_string())
            .is_err()
    );
    invalid["functions"]["test.base"]["limits"] = json!({"unknown":1});
    assert!(TypescriptFunction::from_manifest(worker, "test.base", &invalid.to_string()).is_err());
}
