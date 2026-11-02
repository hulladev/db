use hulladb_postgres::managed::{
    Authorization, Context, Error, FunctionContract, FunctionFuture, Invocation, ManagedSchema,
    ManagedStore, Mutation, MutationContext, QueryContext, Registry, Roles, ScopeQuota, Select,
};
use hulladb_schema::{
    FieldDefinition, RecordValues, ReferenceDeclaration, ReferenceOnDelete, ScalarType,
    SchemaDeclaration, TableDeclaration, Value,
};
use serde_json::{Value as Json, json};
use uuid::Uuid;

fn url() -> String {
    std::env::var("HULLADB_TEST_URL")
        .unwrap_or_else(|_| "postgresql://db_dev:db_dev@127.0.0.1:55432/db_dev".into())
}
fn declaration() -> SchemaDeclaration {
    let mut parent = FieldDefinition::new("parent", ScalarType::Uuid);
    parent.nullable = true;
    let mut schema = SchemaDeclaration::new(vec![
        TableDeclaration::new(
            "parents",
            vec![FieldDefinition::new("title", ScalarType::String)],
        ),
        TableDeclaration::new(
            "tasks",
            vec![FieldDefinition::new("title", ScalarType::String), parent],
        ),
    ]);
    let mut reference = ReferenceDeclaration::new("tasks", "parent", "parents");
    reference.on_delete = ReferenceOnDelete::SetNull;
    schema.references.push(reference);
    schema
}
fn expanded() -> SchemaDeclaration {
    let mut schema = declaration();
    let mut priority = FieldDefinition::new("priority", ScalarType::Int64);
    priority.default = Some(Value::Int64(5));
    priority.constraints.min = Some(Value::Int64(1));
    schema.tables[1].fields.insert(0, priority);
    let mut note = FieldDefinition::new("note", ScalarType::String);
    note.nullable = true;
    schema.tables[1].fields.push(note);
    let mut settings = FieldDefinition::new("settings", ScalarType::Json);
    settings.default = Some(Value::Json(json!({"quote": "'\\; --", "items": [1,true]})));
    schema.tables[1].fields.push(settings);
    schema.tables.push(TableDeclaration::new(
        "audit",
        vec![FieldDefinition::new("task", ScalarType::Uuid)],
    ));
    schema
        .references
        .push(ReferenceDeclaration::new("audit", "task", "tasks"));
    schema
}
fn layout(namespace: &str, schema: SchemaDeclaration) -> ManagedSchema {
    ManagedSchema::new(namespace, schema.normalize().unwrap()).unwrap()
}
async fn fixture() -> (ManagedStore, Context<()>, Value, Value) {
    let namespace = format!("migrate_{}", Uuid::now_v7().simple());
    let mut store = ManagedStore::connect(&url(), layout(&namespace, declaration()))
        .await
        .unwrap();
    store.initialize().await.unwrap();
    let context = Context {
        scope: Uuid::now_v7(),
        principal: Uuid::now_v7(),
        authorization_scope: Uuid::now_v7(),
        identity: (),
    };
    store.create_scope(context.scope).await.unwrap();
    let ((parent, task), _) = store
        .direct_transaction(context.scope, true, |_, write| {
            Box::pin(async move {
                let write = write.unwrap();
                let parent = write
                    .insert(
                        "parents",
                        RecordValues::from([("title".into(), Value::String("Parent".into()))]),
                    )
                    .await?;
                let task = write
                    .insert(
                        "tasks",
                        RecordValues::from([
                            ("title".into(), Value::String("Keep me".into())),
                            ("parent".into(), parent.values["id"].clone()),
                        ]),
                    )
                    .await?;
                Ok((
                    (parent.values["id"].clone(), task.values["id"].clone()),
                    true,
                ))
            })
        })
        .await
        .unwrap();
    (store, context, parent, task)
}
async fn admin() -> (
    tokio_postgres::Client,
    tokio::task::JoinHandle<Result<(), tokio_postgres::Error>>,
) {
    let (client, connection) = tokio_postgres::connect(&url(), tokio_postgres::NoTls)
        .await
        .unwrap();
    (client, tokio::spawn(connection))
}
async fn cleanup(namespace: &str) {
    let (client, connection) = admin().await;
    client
        .batch_execute(&format!("DROP SCHEMA \"{namespace}\" CASCADE"))
        .await
        .unwrap();
    connection.abort();
}
struct Insert;
impl FunctionContract for Insert {
    fn validate_input(&self, _: &Json) -> Result<(), Error> {
        Ok(())
    }
    fn validate_result(&self, _: &Json) -> Result<(), Error> {
        Ok(())
    }
}
impl Mutation<()> for Insert {
    fn call<'a>(
        &'a self,
        context: MutationContext<'a, ()>,
        _: &'a Json,
    ) -> FunctionFuture<'a, Json> {
        Box::pin(async move {
            context
                .db
                .insert(
                    "tasks",
                    RecordValues::from([
                        ("title".into(), Value::String("Receipt row".into())),
                        ("parent".into(), Value::Null),
                    ]),
                )
                .await?;
            context.db.enqueue("created", json!({"kept":true})).await?;
            Ok(json!("original result"))
        })
    }
}
struct Permit;
impl Authorization<()> for Permit {
    fn authorize<'a>(
        &'a self,
        _: QueryContext<'a, ()>,
        _: Invocation,
        _: &'a Json,
    ) -> FunctionFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
}

#[tokio::test]
#[ignore = "requires PostgreSQL; preserves populated data, recovery evidence and constraints"]
async fn additive_upgrade_preserves_rows_ids_receipts_outbox_and_reference_versions() {
    let (mut old, context, parent, task) = fixture().await;
    let namespace = old.schema().namespace().to_owned();
    let mut registry = Registry::new();
    registry.mutation("insert", Insert, Permit).unwrap();
    let request = Uuid::now_v7();
    let receipt = old
        .mutate(&registry, &context, request, "insert", Json::Null)
        .await
        .unwrap();
    let head = old.managed_head(&context).await.unwrap();
    let mut new = ManagedStore::connect(&url(), layout(&namespace, expanded()))
        .await
        .unwrap();
    assert!(matches!(new.verify().await, Err(Error::SchemaMismatch)));
    let plan = new.plan_migration().await.unwrap();
    assert!(!plan.changes().is_empty());
    assert_eq!(
        old.managed_head(&context).await.unwrap(),
        head,
        "inspection is read-only"
    );
    new.apply_migration(&plan).await.unwrap();
    let migrated_head = new.managed_head(&context).await.unwrap();
    assert_eq!(migrated_head.0, head.0);
    assert_eq!(migrated_head.1.sequence, head.1.sequence + 1);
    assert!(
        matches!(old.managed_head(&context).await, Err(Error::SchemaMismatch)),
        "idle old sessions cannot execute a stale layout"
    );
    assert_eq!(new.receipt(&context, request).await.unwrap(), Some(receipt));
    let key = task.clone();
    let (row, _) = new
        .direct_transaction(context.scope, false, |read, _| {
            Box::pin(async move { Ok((read.get("tasks", key).await?.unwrap(), true)) })
        })
        .await
        .unwrap();
    assert_eq!(row.values["title"], Value::String("Keep me".into()));
    assert_eq!(row.values["id"], task);
    assert_eq!(row.values["parent"], parent.clone());
    assert_eq!(row.values["priority"], Value::Int64(5));
    assert_eq!(row.values["note"], Value::Null);
    assert_eq!(
        row.values["settings"],
        Value::Json(json!({"quote":"'\\; --","items":[1,true]}))
    );
    let version = row.version;
    let (client, connection) = admin().await;
    assert_eq!(
        client
            .query_one(&format!("SELECT count(*) FROM \"{namespace}\".outbox"), &[])
            .await
            .unwrap()
            .get::<_, i64>(0),
        1
    );
    // A renamed reference column must still execute its version-maintaining trigger.
    client
        .execute(
            &format!("DELETE FROM \"{namespace}\".t0 WHERE f1=$1"),
            &[&match parent {
                Value::Uuid(value) => Uuid::from_bytes(value),
                _ => panic!(),
            }],
        )
        .await
        .unwrap();
    let (row, _) = new
        .direct_transaction(context.scope, false, |read, _| {
            Box::pin(async move { Ok((read.get("tasks", task).await?.unwrap(), true)) })
        })
        .await
        .unwrap();
    assert_eq!(row.values["parent"], Value::Null);
    assert_eq!(row.version, version + 1);
    assert!(new.plan_migration().await.unwrap().changes().is_empty());
    let current = new.plan_migration().await.unwrap();
    new.apply_migration(&current).await.unwrap();
    assert_eq!(
        new.managed_head(&context).await.unwrap(),
        migrated_head,
        "no-op apply does not advance revisions"
    );
    assert!(
        matches!(new.apply_migration(&plan).await, Err(Error::SchemaMismatch)),
        "stale plans cannot replay DDL"
    );
    connection.abort();
    cleanup(&namespace).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL; incompatible plans leave the original deployment usable"]
async fn destructive_changes_are_rejected_without_changing_data() {
    let (mut old, context, _, _) = fixture().await;
    let namespace = old.schema().namespace().to_owned();
    let head = old.managed_head(&context).await.unwrap();
    let mut candidates = Vec::new();
    let mut changed = declaration();
    changed.tables.remove(1);
    changed.references.clear();
    candidates.push(changed);
    let mut changed = declaration();
    changed.tables.swap(0, 1);
    candidates.push(changed);
    let mut changed = declaration();
    changed.tables[1].fields.remove(0);
    candidates.push(changed);
    let mut changed = declaration();
    changed.tables[1].fields[0].name = "renamed".into();
    candidates.push(changed);
    let mut changed = declaration();
    changed.tables[1].fields[0].scalar_type = ScalarType::Boolean;
    candidates.push(changed);
    let mut changed = declaration();
    changed.tables[1]
        .fields
        .push(FieldDefinition::new("required", ScalarType::String));
    candidates.push(changed);
    for changed in candidates {
        let mut target = ManagedStore::connect(&url(), layout(&namespace, changed))
            .await
            .unwrap();
        assert!(matches!(
            target.plan_migration().await,
            Err(Error::InvalidInput(_))
        ));
        old.verify().await.unwrap();
        assert_eq!(old.managed_head(&context).await.unwrap(), head);
    }
    cleanup(&namespace).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL; failed backfill rolls DDL, ordinal remapping and metadata back together"]
async fn failed_upgrade_rolls_back_added_columns_and_preserves_quotas() {
    let (mut old, context, _, _) = fixture().await;
    let namespace = old.schema().namespace().to_owned();
    let (client, connection) = admin().await;
    let bytes: i64 = client
        .query_one(
            &format!("SELECT pg_column_size(t)::bigint FROM \"{namespace}\".t0 t"),
            &[],
        )
        .await
        .unwrap()
        .get(0);
    let task_bytes: i64 = client
        .query_one(
            &format!("SELECT pg_column_size(t)::bigint FROM \"{namespace}\".t1 t"),
            &[],
        )
        .await
        .unwrap()
        .get(0);
    old.set_scope_quota(
        context.scope,
        ScopeQuota {
            max_data_bytes: Some(bytes + task_bytes),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let head = old.managed_head(&context).await.unwrap();
    let mut new = ManagedStore::connect(&url(), layout(&namespace, expanded()))
        .await
        .unwrap();
    let plan = new.plan_migration().await.unwrap();
    assert!(matches!(
        new.apply_migration(&plan).await,
        Err(Error::QuotaExceeded)
    ));
    old.verify().await.unwrap();
    assert_eq!(old.managed_head(&context).await.unwrap(), head);
    assert_eq!(client.query_one("SELECT count(*) FROM information_schema.columns WHERE table_schema=$1 AND table_name='t1'",&[&namespace]).await.unwrap().get::<_,i64>(0),5);
    assert!(
        !client
            .query_one(
                "SELECT to_regclass($1) IS NOT NULL",
                &[&format!("\"{namespace}\".t2")]
            )
            .await
            .unwrap()
            .get::<_, bool>(0)
    );
    let (rows, _) = old
        .direct_transaction(context.scope, false, |read, _| {
            Box::pin(async move { Ok((read.select("tasks", Select::default()).await?, true)) })
        })
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values["title"], Value::String("Keep me".into()));
    connection.abort();
    cleanup(&namespace).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL; new tables retain the runtime/backup/worker privilege boundary"]
async fn new_tables_inherit_managed_application_permissions() {
    let (mut old, _, _, _) = fixture().await;
    let namespace = old.schema().namespace().to_owned();
    let suffix = Uuid::now_v7().simple().to_string();
    let roles = Roles {
        owner: format!("m_owner_{suffix}"),
        runtime: format!("m_runtime_{suffix}"),
        backup: format!("m_backup_{suffix}"),
        worker: format!("m_worker_{suffix}"),
    };
    old.provision_roles(&roles).await.unwrap();
    let mut new = ManagedStore::connect(&url(), layout(&namespace, expanded()))
        .await
        .unwrap();
    let plan = new.plan_migration().await.unwrap();
    new.apply_migration(&plan).await.unwrap();
    let (client, connection) = admin().await;
    for (role, select, insert) in [
        (&roles.runtime, true, true),
        (&roles.backup, true, false),
        (&roles.worker, false, false),
    ] {
        let row = client
            .query_one(
                "SELECT has_table_privilege($1,$2,'SELECT'),has_table_privilege($1,$2,'INSERT')",
                &[role, &format!("\"{namespace}\".t2")],
            )
            .await
            .unwrap();
        assert_eq!(row.get::<_, bool>(0), select);
        assert_eq!(row.get::<_, bool>(1), insert);
    }
    let owner: String = client
        .query_one(
            "SELECT pg_get_userbyid(relowner) FROM pg_class WHERE oid=to_regclass($1)",
            &[&format!("\"{namespace}\".t2")],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(owner, roles.owner);
    cleanup(&namespace).await;
    client
        .batch_execute(&format!(
            "DROP ROLE \"{}\",\"{}\",\"{}\",\"{}\"",
            roles.owner, roles.runtime, roles.backup, roles.worker
        ))
        .await
        .unwrap();
    connection.abort();
}

#[tokio::test]
#[ignore = "requires PostgreSQL; migration must drain transactions before remapping columns"]
async fn upgrade_waits_for_an_old_transaction_before_publishing_the_new_layout() {
    let (mut old, context, parent, _) = fixture().await;
    let namespace = old.schema().namespace().to_owned();
    let mut new = ManagedStore::connect(&url(), layout(&namespace, expanded()))
        .await
        .unwrap();
    let plan = new.plan_migration().await.unwrap();
    let (entered, inside) = tokio::sync::oneshot::channel();
    let (release, wait) = tokio::sync::oneshot::channel();
    let scope = context.scope;
    let transaction = tokio::spawn(async move {
        old.direct_transaction(scope, true, |read, write| {
            Box::pin(async move {
                assert!(read.get("parents", parent).await?.is_some());
                entered.send(()).unwrap();
                wait.await.unwrap();
                write
                    .unwrap()
                    .insert(
                        "tasks",
                        RecordValues::from([
                            (
                                "title".into(),
                                Value::String("Committed before upgrade".into()),
                            ),
                            ("parent".into(), Value::Null),
                        ]),
                    )
                    .await?;
                Ok(((), true))
            })
        })
        .await
        .unwrap();
    });
    inside.await.unwrap();
    let mut upgrade = tokio::spawn(async move {
        new.apply_migration(&plan).await.unwrap();
        new
    });
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(50), &mut upgrade)
            .await
            .is_err()
    );
    release.send(()).unwrap();
    transaction.await.unwrap();
    let mut new = upgrade.await.unwrap();
    let (rows, _) = new
        .direct_transaction(scope, false, |read, _| {
            Box::pin(async move { Ok((read.select("tasks", Select::default()).await?, true)) })
        })
        .await
        .unwrap();
    let row = rows
        .iter()
        .find(|row| row.values["title"] == Value::String("Committed before upgrade".into()))
        .unwrap();
    assert_eq!(row.values["priority"], Value::Int64(5));
    cleanup(&namespace).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL; invalid references roll back and source changes invalidate inspected plans"]
async fn reference_validation_and_source_fences_leave_the_old_contract_usable() {
    let (mut old, context, _, _) = fixture().await;
    let namespace = old.schema().namespace().to_owned();
    let mut invalid = declaration();
    invalid
        .references
        .push(ReferenceDeclaration::new("tasks", "id", "parents"));
    let mut new = ManagedStore::connect(&url(), layout(&namespace, invalid))
        .await
        .unwrap();
    let plan = new.plan_migration().await.unwrap();
    assert!(matches!(
        new.apply_migration(&plan).await,
        Err(Error::ReferenceViolation)
    ));
    old.verify().await.unwrap();
    let mut new = ManagedStore::connect(&url(), layout(&namespace, expanded()))
        .await
        .unwrap();
    let plan = new.plan_migration().await.unwrap();
    let source = old.managed_head(&context).await.unwrap().0;
    old.rotate_source(source).await.unwrap();
    assert!(matches!(
        new.apply_migration(&plan).await,
        Err(Error::SourceMismatch)
    ));
    old.verify().await.unwrap();
    cleanup(&namespace).await;
}
