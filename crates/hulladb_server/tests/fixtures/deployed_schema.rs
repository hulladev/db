// Generated schema by @hulla/db. Do not edit.

pub fn schema() -> Result<hulladb_schema::SchemaContract, hulladb_schema::SchemaError> {
    let tables = vec![
        {
            let mut table = hulladb_schema::TableDeclaration::new("entries", vec![
                hulladb_schema::FieldDefinition { name: "title".into(), scalar_type: hulladb_schema::ScalarType::String, nullable: false, precision: None, json_shape: None, default: None, constraints: hulladb_schema::FieldConstraints { min: None, max: None, one_of: vec![] } },
                hulladb_schema::FieldDefinition { name: "owner".into(), scalar_type: hulladb_schema::ScalarType::String, nullable: false, precision: None, json_shape: None, default: None, constraints: hulladb_schema::FieldConstraints { min: None, max: None, one_of: vec![] } },
                hulladb_schema::FieldDefinition { name: "ordinal".into(), scalar_type: hulladb_schema::ScalarType::Int64, nullable: false, precision: None, json_shape: None, default: Some(hulladb_schema::Value::Int64(0i64)), constraints: hulladb_schema::FieldConstraints { min: None, max: None, one_of: vec![] } },
                hulladb_schema::FieldDefinition { name: "id".into(), scalar_type: hulladb_schema::ScalarType::Uuid, nullable: false, precision: None, json_shape: None, default: None, constraints: hulladb_schema::FieldConstraints { min: None, max: None, one_of: vec![] } },
            ]);
            table.primary_key = Some("id".into());
            table.key_generation = Some(hulladb_schema::KeyGeneration::UuidV7);
            table.insert_key_policy = hulladb_schema::InsertKeyPolicy::AllowExplicit;
            table.unique_constraints.push(hulladb_schema::UniqueConstraintDeclaration { name: "owner_title".into(), fields: vec!["owner".into(), "title".into()], null_policy: Some(hulladb_schema::UniqueNullPolicy::NotDistinct) });
            table
        },
    ];
    let mut schema = hulladb_schema::SchemaDeclaration::new(tables);
    schema.default_uuid_generation = hulladb_schema::UuidGeneration::V7;
    schema.normalize()
}
