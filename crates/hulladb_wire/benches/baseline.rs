//! Fixed fixtures, optimized builds, and batched timings for implemented contracts.
//! Fixture changes must increment FIXTURE_VERSION; new cases may be added freely.
use std::{hint::black_box, time::Instant};

use hulladb_schema::{
    FieldDefinition, RecordValues, ReferenceDeclaration, ScalarType, SchemaDeclaration,
    TableDeclaration, UniqueConstraintDeclaration, Value,
};
use hulladb_wire::{decode_record, encode_record};
use serde_json::{Value as Json, json};

const FIXTURE_VERSION: u32 = 1;
const SAMPLES: usize = 31;
const TARGET_NS: u128 = 5_000_000;

fn record_fixture(count: usize) -> (TableDeclaration, RecordValues) {
    let mut fields = vec![FieldDefinition::new("id", ScalarType::Uuid)];
    let mut values = RecordValues::from([("id".into(), Value::Uuid([0x42; 16]))]);
    for i in 1..count {
        let name = format!("field_{i:03}");
        let (scalar, value) = match i % 3 {
            0 => (ScalarType::Int64, Value::Int64(9_007_199_254_740_993)),
            1 => (
                ScalarType::String,
                Value::String("representative task title or description".into()),
            ),
            _ => (ScalarType::Boolean, Value::Boolean(true)),
        };
        fields.push(FieldDefinition::new(name.clone(), scalar));
        values.insert(name, value);
    }
    (TableDeclaration::new("tasks", fields), values)
}

fn schema_fixture(count: usize) -> SchemaDeclaration {
    let mut tables = Vec::new();
    let mut references = Vec::new();
    for i in 0..count {
        let (mut table, _) = record_fixture(16);
        table.name = format!("table_{i:03}");
        table
            .fields
            .push(FieldDefinition::new("parent_id", ScalarType::Uuid));
        table
            .unique_constraints
            .push(UniqueConstraintDeclaration::new(
                "business_key",
                vec!["field_001".into(), "field_003".into()],
            ));
        references.push(ReferenceDeclaration::new(
            &table.name,
            "parent_id",
            format!("table_{:03}", (i + 1) % count),
        ));
        tables.push(table);
    }
    let mut schema = SchemaDeclaration::new(tables);
    schema.references = references;
    schema
}

// Owned input setup is outside the timer. Results and consumed inputs are dropped
// inside it, so allocation/deallocation by the operation remains measured.
fn batch<I, O>(
    setup: &mut impl FnMut() -> I,
    operation: &mut impl FnMut(I) -> O,
    n: usize,
) -> u128 {
    let inputs: Vec<_> = (0..n).map(|_| setup()).collect();
    let start = Instant::now();
    for input in inputs {
        black_box(operation(black_box(input)));
    }
    start.elapsed().as_nanos()
}

fn measure<I, O>(
    name: &str,
    workload: Json,
    max_batch: usize,
    smoke: bool,
    mut setup: impl FnMut() -> I,
    mut operation: impl FnMut(I) -> O,
) -> Json {
    let mut iterations = 1;
    if !smoke {
        loop {
            let elapsed = batch(&mut setup, &mut operation, iterations);
            if elapsed >= TARGET_NS || iterations >= max_batch {
                break;
            }
            iterations = (iterations * 2).min(max_batch);
        }
        let warmup = Instant::now();
        while warmup.elapsed().as_millis() < 50 {
            batch(&mut setup, &mut operation, iterations);
        }
    }
    let samples: Vec<f64> = (0..if smoke { 1 } else { SAMPLES })
        .map(|_| batch(&mut setup, &mut operation, iterations) as f64 / iterations as f64)
        .collect();
    let median = median(&samples);
    let deviations: Vec<_> = samples
        .iter()
        .map(|sample| (sample - median).abs())
        .collect();
    json!({
        "name": name,
        "fixture_version": FIXTURE_VERSION,
        "workload": workload,
        "iterations_per_sample": iterations,
        "samples_ns": samples,
        "median_ns": median,
        "mad_ns": self::median(&deviations),
    })
}

fn median(values: &[f64]) -> f64 {
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    sorted[sorted.len() / 2]
}

fn main() {
    // Only an explicit benchmark run collects timings; Cargo test stays a smoke check.
    let args: Vec<_> = std::env::args().skip(1).collect();
    assert!(args.iter().all(|arg| arg == "--bench" || arg == "--test"));
    let smoke = !args.iter().any(|arg| arg == "--bench");
    let mut results = Vec::new();
    for fields in [8, 32, 128] {
        let (declaration, row) = record_fixture(fields);
        let table = declaration.normalize().expect("valid fixture");
        let encoded = encode_record(&row).expect("encodable fixture");
        assert_eq!(decode_record(&encoded).expect("decodable fixture"), row);
        table.validate_row(&row).expect("valid fixture row");
        let workload = json!({"fields": fields, "json_bytes": encoded.len()});
        results.push(measure(
            &format!("validate_row/{fields}"),
            workload.clone(),
            65_536,
            smoke,
            || &row,
            |values| table.validate_row(values).expect("valid row"),
        ));
        results.push(measure(
            &format!("decode_record/{fields}"),
            workload.clone(),
            8_192,
            smoke,
            || encoded.as_str(),
            |input| decode_record(input).expect("valid JSON"),
        ));
        results.push(measure(
            &format!("encode_record/{fields}"),
            workload.clone(),
            8_192,
            smoke,
            || &row,
            |values| encode_record(values).expect("encodable row"),
        ));
        results.push(measure(
            &format!("record_pipeline/{fields}"),
            workload.clone(),
            8_192,
            smoke,
            || encoded.as_str(),
            |input| {
                let values = decode_record(input).expect("valid JSON");
                table.validate_row(&values).expect("valid row");
                encode_record(&values).expect("encodable row")
            },
        ));
        let patch =
            RecordValues::from([("field_001".into(), Value::String("updated title".into()))]);
        results.push(measure(
            &format!("validate_patch/{fields}"),
            workload.clone(),
            65_536,
            smoke,
            || &patch,
            |values| table.validate_update(values).expect("valid patch"),
        ));
        results.push(measure(
            &format!("apply_patch/{fields}"),
            workload.clone(),
            256,
            smoke,
            || (row.clone(), patch.clone()),
            |(mut values, patch)| {
                table
                    .apply_update(&mut values, patch)
                    .expect("valid update");
                values
            },
        ));
        let (mut defaults, _) = record_fixture(fields);
        for field in defaults.fields.iter_mut().skip(1) {
            field.default = row.get(&field.name).cloned();
        }
        let defaults = defaults.normalize().expect("valid defaults");
        results.push(measure(
            &format!("prepare_insert_defaults/{fields}"),
            workload,
            256,
            smoke,
            RecordValues::new,
            |values| defaults.prepare_insert(values).expect("valid insert"),
        ));
    }
    for tables in [4, 16, 64] {
        let declaration = schema_fixture(tables);
        results.push(measure(
            &format!("normalize_schema/{tables}"),
            json!({"tables": tables, "fields_per_table": 17, "references": tables, "unique_constraints": tables}),
            64, smoke,
            || declaration.clone(), |schema| schema.normalize().expect("valid schema"),
        ));
    }
    println!("{}", json!({"format_version": 1, "results": results}));
}
