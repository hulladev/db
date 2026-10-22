use hulladb_postgres::managed::{
    Error, ManagedSchema, ManagedStore, Predicate, WriteExpression, WriteSource,
};
use hulladb_schema::{
    CheckDefinition, CheckOperand, FieldConstraints, FieldDefinition, IndexDeclaration,
    RecordValues, ReferenceDeclaration, ReferenceOnDelete, ScalarType, SchemaDeclaration,
    TableDeclaration, Value,
};
use uuid::Uuid;

fn schema() -> SchemaDeclaration {
    let mut label = FieldDefinition::new("label", ScalarType::String);
    label.constraints = FieldConstraints {
        min: Some(Value::Int64(2)),
        max: Some(Value::Int64(3)),
        one_of: vec![Value::String("🦀é".into()), Value::String("ok".into())],
    };
    let mut quantity = FieldDefinition::new("quantity", ScalarType::Int64);
    quantity.constraints = FieldConstraints {
        min: Some(Value::Int64(0)),
        max: Some(Value::Int64(100)),
        one_of: vec![],
    };
    let mut parents = TableDeclaration::new(
        "parents",
        vec![
            FieldDefinition::new("id", ScalarType::Int64),
            label,
            quantity,
            FieldDefinition::new("ceiling", ScalarType::Int64),
        ],
    );
    let mut payload = FieldDefinition::new("payload", ScalarType::Json);
    payload.nullable = true;
    payload.default = Some(Value::Json(serde_json::json!({"marker":["float64",0]})));
    parents.fields.push(payload);
    parents.indexes.push(IndexDeclaration {
        name: "quantity_ceiling".into(),
        fields: vec!["quantity".into(), "ceiling".into()],
    });
    parents.checks.push(CheckDefinition {
        name: "within_ceiling".into(),
        parts: vec!["".into(), " <= ".into(), "".into()],
        operands: vec![
            CheckOperand::Field("quantity".into()),
            CheckOperand::Field("ceiling".into()),
        ],
    });
    parents.checks.push(CheckDefinition {
        name: "quoted_literal".into(),
        parts: vec!["".into(), " <> ".into(), "".into()],
        operands: vec![
            CheckOperand::Field("label".into()),
            CheckOperand::Value(Value::String("bad'\\literal; --".into())),
        ],
    });
    parents.checks.push(CheckDefinition {
        name: "numeric_constant".into(),
        parts: vec!["".into(), " >= ".into(), "".into()],
        operands: vec![
            CheckOperand::Field("quantity".into()),
            CheckOperand::Value(Value::float64(0.0).unwrap()),
        ],
    });
    let cascades = TableDeclaration::new(
        "cascades",
        vec![
            FieldDefinition::new("id", ScalarType::Int64),
            FieldDefinition::new("parent", ScalarType::Int64),
        ],
    );
    let mut nullable = FieldDefinition::new("parent", ScalarType::Int64);
    nullable.nullable = true;
    let clears = TableDeclaration::new(
        "clears",
        vec![FieldDefinition::new("id", ScalarType::Int64), nullable],
    );
    let restricts = TableDeclaration::new(
        "restricts",
        vec![
            FieldDefinition::new("id", ScalarType::Int64),
            FieldDefinition::new("parent", ScalarType::Int64),
        ],
    );
    let mut schema = SchemaDeclaration::new(vec![parents, cascades, clears, restricts]);
    for (table, policy) in [
        ("cascades", ReferenceOnDelete::Cascade),
        ("clears", ReferenceOnDelete::SetNull),
        ("restricts", ReferenceOnDelete::Restrict),
    ] {
        let mut reference = ReferenceDeclaration::new(table, "parent", "parents");
        reference.on_delete = policy;
        schema.references.push(reference);
    }
    schema
}

#[tokio::test]
#[ignore = "requires PostgreSQL; SQL bypass enforcement and atomic reference side effects"]
async fn postgres_enforces_constraints_indexes_and_atomic_delete_policies() {
    let url = std::env::var("HULLADB_TEST_URL")
        .unwrap_or_else(|_| "postgresql://db_dev:db_dev@127.0.0.1:55432/db_dev".into());
    let schema = ManagedSchema::new(
        format!("schema_rules_{}", Uuid::now_v7().simple()),
        schema().normalize().unwrap(),
    )
    .unwrap();
    let mut store = ManagedStore::connect(&url, schema.clone()).await.unwrap();
    store.initialize().await.unwrap();
    let scope = Uuid::now_v7();
    let other = Uuid::now_v7();
    store.create_scope(scope).await.unwrap();
    store.create_scope(other).await.unwrap();
    let (client, connection) = tokio_postgres::connect(&url, tokio_postgres::NoTls)
        .await
        .unwrap();
    let task = tokio::spawn(connection);
    let ns = schema.namespace();
    #[cfg(feature = "typescript")]
    {
        // TypeScript JSON serializes 0.0 as 0. Reusing the same normalized
        // schema from generated Rust and a JS manifest must preserve data.
        let serialized: String = client
            .query_one(&format!("SELECT contract FROM \"{ns}\".metadata"), &[])
            .await
            .unwrap()
            .get(0);
        let mut descriptor: serde_json::Value = serde_json::from_str(&serialized).unwrap();
        descriptor["tables"][0]["checks"][2]["operands"][1]["value"][1] = serde_json::json!(0);
        let frontend =
            hulladb_postgres::managed::typescript::schema_from_descriptor(ns, descriptor.clone())
                .unwrap();
        let mut reopened = ManagedStore::connect(&url, frontend).await.unwrap();
        reopened.initialize().await.unwrap();
        // Arbitrary JSON remains exact even if it happens to resemble a tagged
        // value; semantic float comparison applies only to typed scalar values.
        descriptor["tables"][0]["fields"][4]["default"][1]["marker"][1] = serde_json::json!(0.0);
        let changed =
            hulladb_postgres::managed::typescript::schema_from_descriptor(ns, descriptor).unwrap();
        let mut changed = ManagedStore::connect(&url, changed).await.unwrap();
        assert!(matches!(
            changed.initialize().await.unwrap_err(),
            Error::SchemaMismatch
        ));
    }

    for selected in [scope, other] {
        client.execute(&format!("INSERT INTO \"{ns}\".t0 (_scope,_version,f0,f1,f2,f3) VALUES ($1,1,1,'🦀é',5,10)"),&[&selected]).await.unwrap();
        for table in ["t1", "t2", "t3"] {
            client
                .execute(
                    &format!(
                        "INSERT INTO \"{ns}\".{table} (_scope,_version,f0,f1) VALUES ($1,1,1,1)"
                    ),
                    &[&selected],
                )
                .await
                .unwrap();
        }
    }
    store.direct_transaction(scope,false,|read,_|Box::pin(async move {
        let filter:Predicate=serde_json::from_value(serde_json::json!({"kind":"compare","field":"quantity","operator":"<","operand":{"kind":"value","value":"-1"}})).unwrap();
        assert!(read.select_where("parents",&[filter],None,&[]).await?.is_empty());
        Ok(((),true))
    })).await.unwrap();
    // Invalid values bypass all SDK validation, proving real PostgreSQL CHECKs.
    for assignment in ["f1='🦀'", "f1='ab'", "f2=-1", "f2=101", "f2=11"] {
        let error = client
            .execute(
                &format!("UPDATE \"{ns}\".t0 SET {assignment} WHERE _scope=$1"),
                &[&scope],
            )
            .await
            .unwrap_err();
        assert_eq!(
            error.code(),
            Some(&tokio_postgres::error::SqlState::CHECK_VIOLATION)
        );
    }
    let index:bool=client.query_one("SELECT EXISTS(SELECT 1 FROM pg_indexes WHERE schemaname=$1 AND tablename='t0' AND indexdef LIKE '%(_scope, f2, f3)%')",&[&ns]).await.unwrap().get(0);
    assert!(index);
    let error = store
        .direct_transaction(scope, true, |_, write| {
            Box::pin(async move {
                let write = write.unwrap();
                write
                    .update_expressions_projected(
                        "parents",
                        &[Predicate::Constant { value: true }],
                        std::collections::BTreeMap::from([(
                            "quantity".into(),
                            WriteExpression::Add {
                                left: Box::new(WriteExpression::Field {
                                    source: WriteSource::Current,
                                    field: "quantity".into(),
                                }),
                                right: Box::new(WriteExpression::Value {
                                    value: serde_json::json!("-1"),
                                }),
                            },
                        )]),
                        None,
                    )
                    .await?;
                // An earlier valid decrement must roll back with the later computed CHECK failure.
                write
                    .update_expressions_projected(
                        "parents",
                        &[Predicate::Constant { value: true }],
                        std::collections::BTreeMap::from([(
                            "quantity".into(),
                            WriteExpression::Add {
                                left: Box::new(WriteExpression::Field {
                                    source: WriteSource::Current,
                                    field: "quantity".into(),
                                }),
                                right: Box::new(WriteExpression::Value {
                                    value: serde_json::json!("10"),
                                }),
                            },
                        )]),
                        None,
                    )
                    .await?;
                Ok(((), true))
            })
        })
        .await
        .unwrap_err();
    assert!(matches!(error, Error::InvalidInput(_)));
    let quantity: i64 = client
        .query_one(
            &format!("SELECT f2 FROM \"{ns}\".t0 WHERE _scope=$1"),
            &[&scope],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(quantity, 5);
    let initial: i64 = client
        .query_one(
            &format!("SELECT sequence FROM \"{ns}\".scopes WHERE id=$1"),
            &[&scope],
        )
        .await
        .unwrap()
        .get(0);
    let error = store
        .direct_transaction(scope, true, |_, write| {
            Box::pin(async move {
                write.unwrap().delete("parents", Value::Int64(1), 1).await?;
                Ok(((), true))
            })
        })
        .await
        .unwrap_err();
    assert!(matches!(error, Error::ReferenceViolation), "{error:?}");
    // A restrict failure must undo any cascade/nulling already attempted by FK triggers.
    let current: (i64, i64) = {
        let row=client.query_one(&format!("SELECT (SELECT count(*) FROM \"{ns}\".t1 WHERE _scope=$1),(SELECT f1 FROM \"{ns}\".t2 WHERE _scope=$1)"),&[&scope]).await.unwrap();
        (row.get(0), row.get(1))
    };
    assert_eq!(current, (1, 1));
    client
        .execute(
            &format!("DELETE FROM \"{ns}\".t3 WHERE _scope=$1"),
            &[&scope],
        )
        .await
        .unwrap();
    // Explicit transaction rollback restores the parent, cascade rows, nulls and versions.
    store
        .direct_transaction(scope, true, |_, write| {
            Box::pin(async move {
                write.unwrap().delete("parents", Value::Int64(1), 1).await?;
                Ok(((), false))
            })
        })
        .await
        .unwrap();
    let unchanged: i64 = client
        .query_one(
            &format!("SELECT _version FROM \"{ns}\".t2 WHERE _scope=$1"),
            &[&scope],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(unchanged, 1);
    store
        .direct_transaction(scope, true, |_, write| {
            Box::pin(async move {
                write.unwrap().delete("parents", Value::Int64(1), 1).await?;
                Ok(((), true))
            })
        })
        .await
        .unwrap();
    let state=client.query_one(&format!("SELECT (SELECT count(*) FROM \"{ns}\".t1 WHERE _scope=$1),f1,_version,(SELECT sequence FROM \"{ns}\".scopes WHERE id=$1) FROM \"{ns}\".t2 WHERE _scope=$1"),&[&scope]).await.unwrap();
    assert_eq!(state.get::<_, i64>(0), 0);
    assert_eq!(state.get::<_, Option<i64>>(1), None);
    assert_eq!(state.get::<_, i64>(2), 2);
    assert_eq!(state.get::<_, i64>(3), initial + 1);
    let other_count: i64 = client
        .query_one(
            &format!("SELECT count(*) FROM \"{ns}\".t1 WHERE _scope=$1"),
            &[&other],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(other_count, 1);
    // Managed updates also change _version exactly once, despite the FK trigger.
    store
        .direct_transaction(scope, true, |_, write| {
            Box::pin(async move {
                let write = write.unwrap();
                write
                    .insert(
                        "parents",
                        RecordValues::from([
                            ("id".into(), Value::Int64(2)),
                            ("label".into(), Value::String("ok".into())),
                            ("quantity".into(), Value::Int64(3)),
                            ("ceiling".into(), Value::Int64(9)),
                        ]),
                    )
                    .await?;
                write
                    .update_where(
                        "clears",
                        &[Predicate::Constant { value: true }],
                        RecordValues::from([("parent".into(), Value::Int64(2))]),
                    )
                    .await?;
                Ok(((), true))
            })
        })
        .await
        .unwrap();
    let version: i64 = client
        .query_one(
            &format!("SELECT _version FROM \"{ns}\".t2 WHERE _scope=$1"),
            &[&scope],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(version, 3);
    client
        .batch_execute(&format!("DROP SCHEMA \"{ns}\" CASCADE"))
        .await
        .unwrap();
    task.abort();
}
