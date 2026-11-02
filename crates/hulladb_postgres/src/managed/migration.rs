//! Explicit additive application-schema upgrades. Runtime/storage format upgrades
//! and destructive transformations require a separate, reviewed operator procedure.
use hulladb_schema::Value;
use serde::Serialize;
use serde_json::Value as Json;
use tokio_postgres::Transaction;
use uuid::Uuid;

use super::{
    Error, ManagedSchema, ManagedStore,
    storage::{column_definition, descriptor_equal, sql_literal},
};

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SchemaChange {
    AddTable { table: String },
    ReorderFields { table: String },
    AddField { table: String, field: String },
    AddReference { table: String, field: String },
}

/// Bound to the exact source and installed contract read during planning. Fields
/// are private so a caller cannot turn an inspected plan into arbitrary SQL.
#[derive(Debug, Clone, Serialize)]
pub struct MigrationPlan {
    source: Uuid,
    changes: Vec<SchemaChange>,
    #[serde(skip)]
    installed: Json,
    #[serde(skip)]
    target: Json,
}

impl MigrationPlan {
    pub fn changes(&self) -> &[SchemaChange] {
        &self.changes
    }
}

fn unsupported() -> Error {
    Error::InvalidInput("unsupported schema change: preserve existing table/reference order and definitions; add tables or nullable/defaulted fields; use a reviewed data migration for renames, removals or type/constraint changes".into())
}

fn array<'a>(value: &'a Json, key: &str) -> Result<&'a Vec<Json>, Error> {
    value
        .get(key)
        .and_then(Json::as_array)
        .ok_or(Error::SchemaMismatch)
}

fn changes(installed: &Json, target: &Json) -> Result<Vec<SchemaChange>, Error> {
    let old_tables = array(installed, "tables")?;
    let new_tables = array(target, "tables")?;
    let old_refs = array(installed, "references")?;
    let new_refs = array(target, "references")?;
    let mut prefix = target.clone();
    prefix["tables"] = installed["tables"].clone();
    prefix["references"] = installed["references"].clone();
    if !descriptor_equal(installed, &prefix)
        || old_tables.len() > new_tables.len()
        || old_refs.len() > new_refs.len()
        || !old_refs
            .iter()
            .zip(new_refs)
            .all(|(old, new)| descriptor_equal(old, new))
    {
        return Err(unsupported());
    }
    let mut changes = Vec::new();
    for (index, table) in new_tables.iter().enumerate() {
        let name = table["name"]
            .as_str()
            .ok_or(Error::SchemaMismatch)?
            .to_owned();
        let Some(old) = old_tables.get(index) else {
            changes.push(SchemaChange::AddTable { table: name });
            continue;
        };
        let old_fields = array(old, "fields")?;
        let new_fields = array(table, "fields")?;
        let mut prefix = table.clone();
        prefix["fields"] = old["fields"].clone();
        if !descriptor_equal(old, &prefix)
            || old_fields.len() > new_fields.len()
            || !old_fields
                .iter()
                .all(|old| new_fields.iter().any(|new| descriptor_equal(old, new)))
        {
            return Err(unsupported());
        }
        if old_fields
            .iter()
            .enumerate()
            .any(|(index, old)| old["name"] != new_fields[index]["name"])
        {
            changes.push(SchemaChange::ReorderFields {
                table: name.clone(),
            });
        }
        for field in new_fields
            .iter()
            .filter(|field| !old_fields.iter().any(|old| old["name"] == field["name"]))
        {
            if field["nullable"] != true && field["default"].is_null() {
                return Err(unsupported());
            }
            changes.push(SchemaChange::AddField {
                table: name.clone(),
                field: field["name"]
                    .as_str()
                    .ok_or(Error::SchemaMismatch)?
                    .to_owned(),
            });
        }
    }
    for reference in &new_refs[old_refs.len()..] {
        changes.push(SchemaChange::AddReference {
            table: reference["table"]
                .as_str()
                .ok_or(Error::SchemaMismatch)?
                .to_owned(),
            field: reference["field"]
                .as_str()
                .ok_or(Error::SchemaMismatch)?
                .to_owned(),
        });
    }
    Ok(changes)
}

async fn installed(
    tx: &Transaction<'_>,
    schema: &ManagedSchema,
) -> Result<(Uuid, String, Json), Error> {
    if !tx
        .query_one(
            "SELECT to_regclass($1) IS NOT NULL",
            &[&schema.relation("metadata")],
        )
        .await?
        .get::<_, bool>(0)
    {
        return Err(Error::SchemaMismatch);
    }
    super::outbox::verify_layout(tx, schema).await?;
    let row = tx
        .query_opt(
            &format!(
                "SELECT source,contract FROM {} WHERE singleton",
                schema.relation("metadata")
            ),
            &[],
        )
        .await?
        .ok_or(Error::SchemaMismatch)?;
    let descriptor: String = row.try_get(1)?;
    Ok((
        row.try_get(0)?,
        descriptor.clone(),
        serde_json::from_str(&descriptor).map_err(|_| Error::SchemaMismatch)?,
    ))
}

impl ManagedStore {
    /// Read-only inspection. This never initializes storage or changes data.
    pub async fn plan_migration(&mut self) -> Result<MigrationPlan, Error> {
        let tx = self
            .client
            .build_transaction()
            .read_only(true)
            .start()
            .await?;
        let (source, _, installed) = installed(&tx, &self.schema).await?;
        let target = self.schema.descriptor();
        let changes = changes(&installed, &target)?;
        tx.commit().await?;
        Ok(MigrationPlan {
            source,
            installed,
            target,
            changes,
        })
    }

    /// Apply an inspected plan atomically with a provisioning credential. Drain
    /// application traffic first. A stale plan is rejected under the install lock.
    pub async fn apply_migration(&mut self, plan: &MigrationPlan) -> Result<(), Error> {
        if !descriptor_equal(&plan.target, &self.schema.descriptor()) {
            return Err(Error::SchemaMismatch);
        }
        self.initialized = false;
        let tx = self.client.transaction().await?;
        tx.query_one(
            "SELECT pg_advisory_xact_lock(hashtextextended($1,0))",
            &[&self.schema.namespace],
        )
        .await?;
        // All execution reads metadata before touching business rows. Drain those
        // transactions before DDL; new executions see the committed new contract.
        tx.batch_execute(&format!(
            "LOCK TABLE {} IN ACCESS EXCLUSIVE MODE",
            self.schema.relation("metadata")
        ))
        .await?;
        let (source, mut encoded, current) = installed(&tx, &self.schema).await?;
        if source != plan.source {
            return Err(Error::SourceMismatch);
        }
        if !descriptor_equal(&current, &plan.installed) {
            return Err(Error::SchemaMismatch);
        }
        let expected = changes(&current, &plan.target)?;
        if expected != plan.changes {
            return Err(Error::SchemaMismatch);
        }
        let old_tables = array(&current, "tables")?;
        let old_refs = array(&current, "references")?;
        for (index, old) in old_tables.iter().enumerate() {
            let table = &self.schema.contract.tables()[index];
            let old_fields = array(old, "fields")?;
            let relation = self.schema.table_relation(table);
            let reordered = old_fields
                .iter()
                .enumerate()
                .any(|(index, old)| old["name"] != table.fields()[index].name);
            if reordered {
                // Two phases avoid collisions when names exchange physical slots.
                for index in 0..old_fields.len() {
                    tx.batch_execute(&format!(
                        "ALTER TABLE {relation} RENAME COLUMN f{index} TO migration_f{index}"
                    ))
                    .await?;
                }
                for (index, old) in old_fields.iter().enumerate() {
                    let ordinal = table
                        .fields()
                        .iter()
                        .position(|field| old["name"] == field.name)
                        .ok_or(Error::SchemaMismatch)?;
                    tx.batch_execute(&format!(
                        "ALTER TABLE {relation} RENAME COLUMN migration_f{index} TO f{ordinal}"
                    ))
                    .await?;
                }
            }
            let fields: Vec<_> = table
                .fields()
                .iter()
                .enumerate()
                .filter(|(_, field)| !old_fields.iter().any(|old| old["name"] == field.name))
                .collect();
            for (ordinal, field) in &fields {
                let mut definition = column_definition(*ordinal, field)?;
                if let Some(value) = &field.default {
                    let literal = match value {
                        Value::Json(value) => {
                            format!("{}::jsonb", sql_literal(&Value::String(value.to_string()))?)
                        }
                        value => sql_literal(value)?,
                    };
                    definition.push_str(&format!(" DEFAULT {literal}"));
                }
                tx.batch_execute(&format!("ALTER TABLE {relation} ADD COLUMN {definition}; ALTER TABLE {relation} ALTER COLUMN f{ordinal} DROP DEFAULT;")).await?;
            }
            if !fields.is_empty() {
                tx.batch_execute(&format!("UPDATE {relation} SET _version=_version+1"))
                    .await?;
            }
        }
        tx.batch_execute(&self.schema.refresh_reference_functions(old_refs.len())?)
            .await?;
        tx.batch_execute(
            &self
                .schema
                .application_ddl(old_tables.len(), old_refs.len())?,
        )
        .await?;
        // New tables use the existing application table's owner and table grants.
        // With an initially empty schema, infer read and runtime write grants from
        // managed metadata. Column-level custom grants remain operator-owned.
        for table in &self.schema.contract.tables()[old_tables.len()..] {
            inherit_access(
                &tx,
                &self.schema,
                &self.schema.table_relation(table),
                !old_tables.is_empty(),
            )
            .await?;
        }
        if !expected.is_empty() {
            tx.execute(
                &format!(
                    "UPDATE {} SET sequence=sequence+1",
                    self.schema.relation("scopes")
                ),
                &[],
            )
            .await?;
            // Backfilled values count against the existing durable scope quotas.
            for row in tx
                .query(
                    &format!("SELECT id FROM {}", self.schema.relation("scopes")),
                    &[],
                )
                .await?
            {
                super::operations::enforce_quota(&tx, &self.schema, row.get(0), 0).await?;
            }
            encoded = plan.target.to_string();
            tx.execute(
                &format!(
                    "UPDATE {} SET contract=$1 WHERE singleton",
                    self.schema.relation("metadata")
                ),
                &[&encoded],
            )
            .await?;
        }
        tx.commit().await.map_err(super::store::commit_error)?;
        self.schema.installed_descriptor = Some(encoded);
        self.initialized = true;
        Ok(())
    }
}

async fn inherit_access(
    tx: &Transaction<'_>,
    schema: &ManagedSchema,
    relation: &str,
    has_table: bool,
) -> Result<(), Error> {
    let template = schema.relation(if has_table { "t0" } else { "scopes" });
    // pg_catalog.format quotes database-owned identifiers, including externally
    // provisioned role names. Only catalog privilege names enter SQL as keywords.
    let owner: String = tx.query_one("SELECT format('%I',r.rolname) FROM pg_class c JOIN pg_roles r ON r.oid=c.relowner WHERE c.oid=to_regclass($1)", &[&template]).await?.get(0);
    let acl: Vec<String> = tx.query("SELECT DISTINCT CASE WHEN a.grantee=0 THEN 'PUBLIC' ELSE quote_ident(r.rolname) END FROM pg_class c CROSS JOIN LATERAL aclexplode(COALESCE(c.relacl,acldefault('r',c.relowner))) a LEFT JOIN pg_roles r ON r.oid=a.grantee WHERE c.oid=to_regclass($1)", &[&relation]).await?.into_iter().map(|row|row.get(0)).collect();
    for grantee in acl {
        tx.batch_execute(&format!("REVOKE ALL ON {relation} FROM {grantee}"))
            .await?;
    }
    for row in tx.query("SELECT a.privilege_type,CASE WHEN a.grantee=0 THEN 'PUBLIC' ELSE quote_ident(r.rolname) END,a.is_grantable FROM pg_class c CROSS JOIN LATERAL aclexplode(COALESCE(c.relacl,acldefault('r',c.relowner))) a LEFT JOIN pg_roles r ON r.oid=a.grantee WHERE c.oid=to_regclass($1) AND a.grantee<>c.relowner", &[&template]).await? {
        let privilege: String = row.get(0);
        if !has_table && privilege != "SELECT" { continue; }
        let grantee: String = row.get(1);
        tx.batch_execute(&format!("GRANT {privilege} ON {relation} TO {grantee}{}", if row.get::<_,bool>(2) { " WITH GRANT OPTION" } else { "" })).await?;
    }
    if !has_table {
        for row in tx.query("SELECT quote_ident(r.rolname) FROM pg_class c CROSS JOIN LATERAL aclexplode(COALESCE(c.relacl,acldefault('r',c.relowner))) a JOIN pg_roles r ON r.oid=a.grantee WHERE c.oid=to_regclass($1) AND a.grantee<>c.relowner AND a.privilege_type='INSERT'", &[&schema.relation("receipts")]).await? {
            let grantee: String = row.get(0);
            tx.batch_execute(&format!("GRANT INSERT,UPDATE,DELETE ON {relation} TO {grantee}")).await?;
        }
    }
    tx.batch_execute(&format!("ALTER TABLE {relation} OWNER TO {owner}"))
        .await?;
    Ok(())
}
