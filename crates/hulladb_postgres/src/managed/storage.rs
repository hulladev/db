use hulladb_schema::{
    CheckOperand, KeyGeneration, ReferenceOnDelete, ScalarType, SchemaContract, TableContract,
    UniqueNullPolicy, Value,
};
use serde_json::{Value as Json, json};

use super::Error;

/// Compiled physical layout. Logical names are mapped to ordinal identifiers,
/// so application names cannot collide with managed columns or inject SQL.
#[derive(Debug, Clone)]
pub struct ManagedSchema {
    pub(super) namespace: String,
    pub(super) contract: SchemaContract,
}

impl ManagedSchema {
    pub fn new(namespace: impl Into<String>, contract: SchemaContract) -> Result<Self, Error> {
        let namespace = namespace.into();
        if namespace.is_empty()
            || namespace.len() > 63
            || !namespace
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'_')
            || namespace.starts_with("pg_")
            || namespace == "public"
        {
            return Err(Error::InvalidInput("use a dedicated PostgreSQL namespace with 1..63 ASCII letters, digits, or underscores".into()));
        }
        Ok(Self {
            namespace,
            contract,
        })
    }

    pub fn namespace(&self) -> &str {
        &self.namespace
    }
    pub fn contract(&self) -> &SchemaContract {
        &self.contract
    }

    pub(super) fn relation(&self, name: &str) -> String {
        format!("\"{}\".\"{name}\"", self.namespace)
    }

    pub(super) fn table(&self, name: &str) -> Result<&TableContract, Error> {
        self.contract
            .table(name)
            .ok_or_else(|| Error::InvalidInput(format!("unknown table {name:?}")))
    }

    pub(super) fn table_relation(&self, table: &TableContract) -> String {
        let index = self
            .contract
            .tables()
            .iter()
            .position(|t| t.name() == table.name())
            .expect("normalized table");
        self.relation(&format!("t{index}"))
    }

    pub(super) fn column(&self, table: &TableContract, field: &str) -> Result<String, Error> {
        let index = table
            .fields()
            .iter()
            .position(|f| f.name == field)
            .ok_or_else(|| {
                Error::InvalidInput(format!("unknown field {field:?} in {:?}", table.name()))
            })?;
        Ok(format!("f{index}"))
    }

    /// Fresh installation only. Existing layouts are compared against the full
    /// contract, never migrated or silently reset. Defaults/generators remain
    /// managed runtime operations; cross-row constraints live in PostgreSQL.
    pub(super) fn ddl(&self) -> Result<String, Error> {
        let scopes = self.relation("scopes");
        let metadata = self.relation("metadata");
        let receipts = self.relation("receipts");
        let quotas = self.relation("quotas");
        let outbox = self.relation("outbox");
        let mut sql = format!(
            "CREATE SCHEMA \"{}\";
             CREATE TABLE {metadata} (singleton boolean PRIMARY KEY CHECK(singleton), source uuid NOT NULL, contract text NOT NULL);
             CREATE TABLE {scopes} (id uuid PRIMARY KEY, sequence bigint NOT NULL DEFAULT 0 CHECK(sequence >= 0));
             CREATE TABLE {receipts} (scope uuid NOT NULL REFERENCES {scopes}(id), principal uuid NOT NULL, request_id uuid NOT NULL, authorization_scope uuid NOT NULL, operation text NOT NULL, input_digest bytea NOT NULL CHECK(octet_length(input_digest)=32), receipt jsonb NOT NULL, created_at timestamptz NOT NULL DEFAULT clock_timestamp(), expired boolean NOT NULL DEFAULT false, PRIMARY KEY(scope, principal, request_id));
             CREATE INDEX ON {receipts} (scope,created_at,principal,request_id) WHERE NOT expired;
             CREATE TABLE {quotas} (scope uuid PRIMARY KEY REFERENCES {scopes}(id), max_rows bigint CHECK(max_rows >= 0), max_data_bytes bigint CHECK(max_data_bytes >= 0), max_receipt_bytes bigint CHECK(max_receipt_bytes >= 0), max_result_bytes bigint CHECK(max_result_bytes >= 0), max_outbox_jobs bigint CHECK(max_outbox_jobs >= 0), max_outbox_bytes bigint CHECK(max_outbox_bytes >= 0));
             CREATE TABLE {outbox} (id uuid PRIMARY KEY, source uuid NOT NULL, scope uuid NOT NULL REFERENCES {scopes}(id), principal uuid NOT NULL, authorization_scope uuid NOT NULL, request_id uuid NOT NULL, operation text NOT NULL, name text NOT NULL, payload jsonb NOT NULL, max_attempts integer NOT NULL CHECK(max_attempts BETWEEN 1 AND 100), attempts integer NOT NULL DEFAULT 0 CHECK(attempts BETWEEN 0 AND max_attempts), available_at timestamptz NOT NULL DEFAULT clock_timestamp(), lease_token uuid, lease_until timestamptz, failed boolean NOT NULL DEFAULT false, last_error text, CHECK((lease_token IS NULL) = (lease_until IS NULL)));
             CREATE INDEX ON {outbox} (source,available_at,id) WHERE NOT failed;",
            self.namespace
        );
        sql.push_str(&super::admission::ddl(self));
        for table in self.contract.tables() {
            let relation = self.table_relation(table);
            sql.push_str(&format!("CREATE TABLE {relation} (_scope uuid NOT NULL REFERENCES {scopes}(id), _version bigint NOT NULL CHECK(_version > 0)"));
            for (i, field) in table.fields().iter().enumerate() {
                let scalar = match field.scalar_type {
                    ScalarType::Date => "date".to_owned(),
                    ScalarType::Timestamp => {
                        format!("timestamp({}) with time zone", field.precision.unwrap_or(3))
                    }
                    ScalarType::LocalTimestamp => format!(
                        "timestamp({}) without time zone",
                        field.precision.unwrap_or(3)
                    ),
                    ScalarType::Json => "jsonb".to_owned(),
                    ScalarType::Float64 => "double precision".to_owned(),
                    ScalarType::String => "text".to_owned(),
                    ScalarType::Boolean => "boolean".to_owned(),
                    ScalarType::Int64 => "bigint".to_owned(),
                    ScalarType::Uuid => "uuid".to_owned(),
                };
                sql.push_str(&format!(
                    ", f{i} {scalar}{}",
                    if field.nullable { "" } else { " NOT NULL" }
                ));
                if matches!(
                    field.scalar_type,
                    ScalarType::Date | ScalarType::Timestamp | ScalarType::LocalTimestamp
                ) {
                    let suffix = match field.scalar_type {
                        ScalarType::Date => "date",
                        ScalarType::Timestamp => "timestamptz",
                        _ => "timestamp",
                    };
                    let lower = if field.scalar_type == ScalarType::Date {
                        "0001-01-01"
                    } else if field.scalar_type == ScalarType::Timestamp {
                        "0001-01-01T00:00:00Z"
                    } else {
                        "0001-01-01T00:00:00"
                    };
                    let upper = if field.scalar_type == ScalarType::Date {
                        "10000-01-01"
                    } else if field.scalar_type == ScalarType::Timestamp {
                        "10000-01-01T00:00:00Z"
                    } else {
                        "10000-01-01T00:00:00"
                    };
                    sql.push_str(&format!(
                        " CHECK (f{i} >= '{lower}'::{suffix} AND f{i} < '{upper}'::{suffix})"
                    ));
                }
            }
            for (i, field) in table.fields().iter().enumerate() {
                let expression = if field.scalar_type == ScalarType::String {
                    format!("char_length(f{i})")
                } else {
                    format!("f{i}")
                };
                for (bound, operator) in [
                    (&field.constraints.min, ">="),
                    (&field.constraints.max, "<="),
                ] {
                    if let Some(bound) = bound {
                        sql.push_str(&format!(
                            ", CHECK ({expression} {operator} {})",
                            sql_literal(bound)?
                        ));
                    }
                }
                if !field.constraints.one_of.is_empty() {
                    let choices = field
                        .constraints
                        .one_of
                        .iter()
                        .map(sql_literal)
                        .collect::<Result<Vec<_>, _>>()?;
                    sql.push_str(&format!(", CHECK (f{i} IN ({}))", choices.join(",")));
                }
            }
            for (index, check) in table.checks().iter().enumerate() {
                let mut expression = check.parts[0].clone();
                for (operand, part) in check.operands.iter().zip(&check.parts[1..]) {
                    expression.push_str(&match operand {
                        CheckOperand::Field(name) => self.column(table, name)?,
                        CheckOperand::Value(value) => sql_literal(value)?,
                    });
                    expression.push_str(part);
                }
                sql.push_str(&format!(
                    ", CONSTRAINT check_{index} CHECK (({expression}))"
                ));
            }
            sql.push_str(&format!(
                ", PRIMARY KEY (_scope, {})",
                self.column(table, &table.primary_key().name)?
            ));
            for unique in table.unique_constraints() {
                let fields = unique
                    .fields()
                    .iter()
                    .map(|f| self.column(table, f))
                    .collect::<Result<Vec<_>, _>>()?;
                let nulls = match unique.null_policy() {
                    UniqueNullPolicy::Distinct => "DISTINCT",
                    UniqueNullPolicy::NotDistinct => "NOT DISTINCT",
                };
                sql.push_str(&format!(
                    ", UNIQUE NULLS {nulls} (_scope, {})",
                    fields.join(",")
                ));
            }
            sql.push_str(");");
            for definition in table.indexes() {
                let fields = definition
                    .fields
                    .iter()
                    .map(|f| self.column(table, f))
                    .collect::<Result<Vec<_>, _>>()?;
                sql.push_str(&format!(
                    "CREATE INDEX ON {relation} (_scope,{});",
                    fields.join(",")
                ));
            }
        }
        // After all relations exist, forward and cyclic references are valid.
        for reference in self.contract.references() {
            let table = self.table(reference.table())?;
            let target = self.table(reference.target_table())?;
            let column = self.column(table, reference.field())?;
            let policy = match reference.on_delete() {
                ReferenceOnDelete::Restrict => "RESTRICT".to_owned(),
                ReferenceOnDelete::Cascade => "CASCADE".to_owned(),
                ReferenceOnDelete::SetNull => format!("SET NULL ({column})"),
            };
            if reference.on_delete() == ReferenceOnDelete::SetNull {
                let index = self
                    .contract
                    .references()
                    .iter()
                    .position(|r| std::ptr::eq(r, reference))
                    .expect("reference");
                let function = self.relation(&format!("null_reference_{index}"));
                sql.push_str(&format!("CREATE FUNCTION {function}() RETURNS trigger LANGUAGE plpgsql AS $hulla$ BEGIN IF NEW._version = OLD._version AND NEW.{column} IS DISTINCT FROM OLD.{column} THEN NEW._version := OLD._version + 1; END IF; RETURN NEW; END $hulla$; CREATE TRIGGER null_reference_{index} BEFORE UPDATE OF {column} ON {} FOR EACH ROW EXECUTE FUNCTION {function}();", self.table_relation(table)));
            }
            sql.push_str(&format!(
                "ALTER TABLE {} ADD FOREIGN KEY (_scope,{}) REFERENCES {} (_scope,{}) ON DELETE {policy};",
                self.table_relation(table), self.column(table, reference.field())?,
                self.table_relation(target), self.column(target, reference.target_field())?,
            ));
        }
        Ok(sql)
    }

    /// Full contract equality guard; this is not a schema generation or upgrade
    /// version. Ordinal layout and runtime defaults must agree on reconnect.
    pub(super) fn descriptor(&self) -> Json {
        json!({
            "uuid_default": format!("{:?}", self.contract.default_uuid_generation()),
            "tables": self.contract.tables().iter().map(|table| json!({
                "name": table.name(),
                "key": table.primary_key().name,
                "generation": match table.key_generation() {
                    KeyGeneration::Custom { handler, output_type } => json!({"custom": handler, "output": format!("{output_type:?}")}),
                    other => json!(format!("{other:?}")),
                },
                "insert_policy": format!("{:?}", table.insert_key_policy()),
                "fields": table.fields().iter().map(|f| { let mut field = json!({
                    "name": f.name, "scalar": format!("{:?}", f.scalar_type), "nullable": f.nullable,
                    "default": f.default.as_ref().map(describe_value),
                    "constraints": describe_constraints(&f.constraints),
                }); if let Some(precision) = f.precision { field["precision"] = json!(precision); }
                if let Some(shape) = &f.json_shape { field["jsonShape"] = json!(shape); } field }).collect::<Vec<_>>(),
                "indexes": table.indexes().iter().map(|index| json!({"name":index.name,"fields":index.fields})).collect::<Vec<_>>(),
                "checks": table.checks().iter().map(|check| json!({"name":check.name,"parts":check.parts,"operands":check.operands.iter().map(|operand|match operand { CheckOperand::Field(field)=>json!({"field":field}),CheckOperand::Value(value)=>json!({"value":describe_value(value)}) }).collect::<Vec<_>>() })).collect::<Vec<_>>(),
                "unique": table.unique_constraints().iter().map(|u| json!({
                    "name": u.name(), "fields": u.fields(), "nulls": format!("{:?}", u.null_policy()),
                })).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
            "references": self.contract.references().iter().map(|r| json!({
                "table": r.table(), "field": r.field(), "target": r.target_table(), "key": r.target_field(), "onDelete":format!("{:?}",r.on_delete()),
            })).collect::<Vec<_>>(),
        })
    }
}

fn describe_value(value: &Value) -> Json {
    match value {
        Value::Json(value) => json!(["json", value]),
        Value::Float64(value) => json!(["float64", value]),
        Value::Null => json!(["null"]),
        Value::String(value) => json!(["string", value]),
        Value::Date(value) => json!(["date", value]),
        Value::Timestamp(value) => json!(["timestamp", value]),
        Value::LocalTimestamp(value) => json!(["localTimestamp", value]),
        Value::Boolean(value) => json!(["boolean", value]),
        Value::Int64(value) => json!(["int64", value.to_string()]),
        Value::Uuid(value) => json!(["uuid", uuid::Uuid::from_bytes(*value).to_string()]),
    }
}

fn describe_constraints(constraints: &hulladb_schema::FieldConstraints) -> Json {
    let mut result =
        json!({"oneOf":constraints.one_of.iter().map(describe_value).collect::<Vec<_>>()});
    if let Some(min) = &constraints.min {
        result["min"] = describe_value(min);
    }
    if let Some(max) = &constraints.max {
        result["max"] = describe_value(max);
    }
    result
}

fn sql_literal(value: &Value) -> Result<String, Error> {
    let quoted = |s: &str| -> Result<String, Error> {
        if s.contains('\0') {
            return Err(Error::InvalidInput("SQL check literal contains NUL".into()));
        }
        Ok(format!("E'{}'", s.replace("\\", "\\\\").replace("'", "''")))
    };
    Ok(match value {
        Value::Null => "NULL".into(),
        Value::Int64(n) => n.to_string(),
        Value::Float64(n) => format!("{}::double precision", n),
        Value::Boolean(b) => b.to_string(),
        Value::String(s) => quoted(s)?,
        Value::Uuid(b) => format!("{}::uuid", quoted(&uuid::Uuid::from_bytes(*b).to_string())?),
        Value::Date(s) => format!("{}::date", quoted(s)?),
        Value::Timestamp(s) => format!("{}::timestamptz", quoted(s)?),
        Value::LocalTimestamp(s) => format!("{}::timestamp", quoted(s)?),
        Value::Json(_) => {
            return Err(Error::InvalidInput(
                "JSON SQL check operands are unsupported".into(),
            ));
        }
    })
}

/// JSON preserves integer-vs-float syntax, but Float64 has one storage meaning.
/// Keep all other values strict, particularly arbitrary JSON and int64 text.
pub(super) fn descriptor_equal(left: &Json, right: &Json) -> bool {
    match (left, right) {
        (Json::Array(left), Json::Array(right))
            if left.first().is_some_and(|tag| tag == "json")
                && right.first().is_some_and(|tag| tag == "json") =>
        {
            left == right
        }
        (Json::Array(left), Json::Array(right))
            if left.len() == 2
                && right.len() == 2
                && left[0] == "float64"
                && right[0] == "float64"
                && left[1].is_number()
                && right[1].is_number() =>
        {
            left[1]
                .as_f64()
                .zip(right[1].as_f64())
                .is_some_and(|(left, right)| left == right)
        }
        (Json::Array(left), Json::Array(right)) => {
            left.len() == right.len()
                && left
                    .iter()
                    .zip(right)
                    .all(|(left, right)| descriptor_equal(left, right))
        }
        (Json::Object(left), Json::Object(right)) => {
            left.len() == right.len()
                && left.iter().all(|(key, left)| {
                    right
                        .get(key)
                        .is_some_and(|right| descriptor_equal(left, right))
                })
        }
        _ => left == right,
    }
}
