// Generated schema by @hulla/db. Do not edit.

pub fn schema() -> Result<hulladb_schema::SchemaContract, hulladb_schema::SchemaError> {
    let tables = vec![
        {
            let mut table = hulladb_schema::TableDeclaration::new("publications", vec![
                hulladb_schema::FieldDefinition { name: "slug".into(), scalar_type: hulladb_schema::ScalarType::String, nullable: false, precision: None, json_shape: None, default: None, constraints: hulladb_schema::FieldConstraints { min: None, max: None, one_of: vec![] } },
                hulladb_schema::FieldDefinition { name: "title".into(), scalar_type: hulladb_schema::ScalarType::String, nullable: false, precision: None, json_shape: None, default: None, constraints: hulladb_schema::FieldConstraints { min: None, max: None, one_of: vec![] } },
            ]);
            table.primary_key = Some("slug".into());
            table.key_generation = Some(hulladb_schema::KeyGeneration::None);
            table.insert_key_policy = hulladb_schema::InsertKeyPolicy::AllowExplicit;
            table
        },
        {
            let mut table = hulladb_schema::TableDeclaration::new("articles", vec![
                hulladb_schema::FieldDefinition { name: "publication".into(), scalar_type: hulladb_schema::ScalarType::String, nullable: false, precision: None, json_shape: None, default: None, constraints: hulladb_schema::FieldConstraints { min: None, max: None, one_of: vec![] } },
                hulladb_schema::FieldDefinition { name: "title".into(), scalar_type: hulladb_schema::ScalarType::String, nullable: false, precision: None, json_shape: None, default: None, constraints: hulladb_schema::FieldConstraints { min: Some(hulladb_schema::Value::Int64(1i64)), max: Some(hulladb_schema::Value::Int64(200i64)), one_of: vec![] } },
                hulladb_schema::FieldDefinition { name: "published".into(), scalar_type: hulladb_schema::ScalarType::Boolean, nullable: false, precision: None, json_shape: None, default: Some(hulladb_schema::Value::Boolean(false)), constraints: hulladb_schema::FieldConstraints { min: None, max: None, one_of: vec![] } },
                hulladb_schema::FieldDefinition { name: "excerpt".into(), scalar_type: hulladb_schema::ScalarType::String, nullable: true, precision: None, json_shape: None, default: None, constraints: hulladb_schema::FieldConstraints { min: None, max: None, one_of: vec![] } },
                hulladb_schema::FieldDefinition { name: "ordinal".into(), scalar_type: hulladb_schema::ScalarType::Int64, nullable: false, precision: None, json_shape: None, default: Some(hulladb_schema::Value::Int64(9223372036854775807i64)), constraints: hulladb_schema::FieldConstraints { min: None, max: None, one_of: vec![] } },
                hulladb_schema::FieldDefinition { name: "minimum".into(), scalar_type: hulladb_schema::ScalarType::Int64, nullable: false, precision: None, json_shape: None, default: Some(hulladb_schema::Value::Int64(-9223372036854775808i64)), constraints: hulladb_schema::FieldConstraints { min: None, max: None, one_of: vec![] } },
                hulladb_schema::FieldDefinition { name: "origin".into(), scalar_type: hulladb_schema::ScalarType::Uuid, nullable: false, precision: None, json_shape: None, default: Some(hulladb_schema::Value::Uuid([0xaa, 0xaa, 0xaa, 0xaa, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01])), constraints: hulladb_schema::FieldConstraints { min: None, max: None, one_of: vec![] } },
                hulladb_schema::FieldDefinition { name: "label".into(), scalar_type: hulladb_schema::ScalarType::String, nullable: false, precision: None, json_shape: None, default: Some(hulladb_schema::Value::String("line\u{a}\"quote\"\\path\u{0}🦀".into())), constraints: hulladb_schema::FieldConstraints { min: None, max: None, one_of: vec![] } },
                hulladb_schema::FieldDefinition { name: "id".into(), scalar_type: hulladb_schema::ScalarType::Uuid, nullable: false, precision: None, json_shape: None, default: None, constraints: hulladb_schema::FieldConstraints { min: None, max: None, one_of: vec![] } },
            ]);
            table.primary_key = Some("id".into());
            table.key_generation = Some(hulladb_schema::KeyGeneration::UuidV4);
            table.insert_key_policy = hulladb_schema::InsertKeyPolicy::GeneratedOnly;
            table.unique_constraints.push(hulladb_schema::UniqueConstraintDeclaration { name: "publication_title".into(), fields: vec!["publication".into(), "title".into()], null_policy: Some(hulladb_schema::UniqueNullPolicy::NotDistinct) });
            table.unique_constraints.push(hulladb_schema::UniqueConstraintDeclaration { name: "excerpt".into(), fields: vec!["excerpt".into()], null_policy: Some(hulladb_schema::UniqueNullPolicy::Distinct) });
            table.indexes.push(hulladb_schema::IndexDeclaration { name: "articles_publication_published_idx".into(), fields: vec!["publication".into(), "published".into()] });
            table.checks.push(hulladb_schema::CheckDefinition { name: "ordinal_range".into(), parts: vec!["".into(), " >= ".into(), " AND ".into(), " >= ".into(), "".into()], operands: vec![hulladb_schema::CheckOperand::Field("ordinal".into()), hulladb_schema::CheckOperand::Field("minimum".into()), hulladb_schema::CheckOperand::Field("ordinal".into()), hulladb_schema::CheckOperand::Value(hulladb_schema::Value::Int64(0i64))] });
            table
        },
        {
            let mut table = hulladb_schema::TableDeclaration::new("counters", vec![
                hulladb_schema::FieldDefinition { name: "id".into(), scalar_type: hulladb_schema::ScalarType::Int64, nullable: false, precision: None, json_shape: None, default: None, constraints: hulladb_schema::FieldConstraints { min: None, max: None, one_of: vec![] } },
                hulladb_schema::FieldDefinition { name: "value".into(), scalar_type: hulladb_schema::ScalarType::Int64, nullable: false, precision: None, json_shape: None, default: None, constraints: hulladb_schema::FieldConstraints { min: None, max: None, one_of: vec![] } },
            ]);
            table.primary_key = Some("id".into());
            table.key_generation = Some(hulladb_schema::KeyGeneration::Custom { handler: "counters.next".into(), output_type: hulladb_schema::ScalarType::Int64 });
            table.insert_key_policy = hulladb_schema::InsertKeyPolicy::AllowExplicit;
            table
        },
    ];
    let mut schema = hulladb_schema::SchemaDeclaration::new(tables);
    schema.default_uuid_generation = hulladb_schema::UuidGeneration::V4;
    { let mut reference = hulladb_schema::ReferenceDeclaration::new("articles", "publication", "publications"); reference.on_delete = hulladb_schema::ReferenceOnDelete::Cascade; schema.references.push(reference); }
    schema.normalize()
}
