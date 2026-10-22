use super::{Error, ManagedSchema, ManagedStore};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tokio_postgres::Transaction;
use uuid::Uuid;

/// Limits are measured inside each managed mutation, including its receipt.
/// Data/receipt bytes use PostgreSQL's pg_column_size of stored tuples, excluding
/// indexes, WAL, page overhead and backups. Outbox bytes count stored JSON text
/// and job names; jobs include dead letters and old-source work until removed.
/// None disables an individual limit.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScopeQuota {
    pub max_rows: Option<i64>,
    pub max_data_bytes: Option<i64>,
    pub max_receipt_bytes: Option<i64>,
    pub max_result_bytes: Option<i64>,
    pub max_outbox_jobs: Option<i64>,
    pub max_outbox_bytes: Option<i64>,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Usage {
    pub rows: i64,
    pub data_bytes: i64,
    pub receipt_bytes: i64,
    pub outbox_jobs: i64,
    pub outbox_bytes: i64,
}

/// Operator snapshot. Counts and ages contain no function inputs, results or job payloads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ScopeStatus {
    pub source: Uuid,
    pub sequence: i64,
    pub quota: ScopeQuota,
    pub usage: Usage,
    pub receipts: ReceiptStatus,
    pub outbox: OutboxStatus,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReceiptStatus {
    pub retained: i64,
    pub expired: i64,
    pub oldest_retained_age_ms: Option<i64>,
}
/// Disjoint states: old-source work is excluded from all current-source counts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OutboxStatus {
    pub ready: i64,
    pub delayed: i64,
    pub leased: i64,
    pub dead: i64,
    pub old_source: i64,
    pub oldest_ready_age_ms: Option<i64>,
}

/// Dedicated NOLOGIN groups. Existing groups must be unprivileged and have no
/// inherited memberships. Provisioning never modifies login/password settings.
#[derive(Debug, Clone)]
pub struct Roles {
    pub owner: String,
    pub runtime: String,
    pub backup: String,
    pub worker: String,
}

pub(super) fn identifier(value: &str) -> Result<String, Error> {
    if value.is_empty()
        || value.len() > 63
        || value.starts_with("pg_")
        || !value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_')
    {
        return Err(Error::InvalidInput(
            "use a dedicated ASCII PostgreSQL identifier of 1..63 characters".into(),
        ));
    }
    Ok(format!("\"{value}\""))
}
impl ScopeQuota {
    fn validate(&self) -> Result<(), Error> {
        if [
            self.max_rows,
            self.max_data_bytes,
            self.max_receipt_bytes,
            self.max_result_bytes,
            self.max_outbox_jobs,
            self.max_outbox_bytes,
        ]
        .into_iter()
        .flatten()
        .any(|v| v < 0)
        {
            return Err(Error::InvalidInput(
                "quota limits must be nonnegative".into(),
            ));
        }
        Ok(())
    }
    fn check(&self, usage: Usage, result_bytes: Option<i64>) -> Result<(), Error> {
        if self.max_rows.is_some_and(|max| usage.rows > max)
            || self
                .max_data_bytes
                .is_some_and(|max| usage.data_bytes > max)
            || self
                .max_receipt_bytes
                .is_some_and(|max| usage.receipt_bytes > max)
            || self
                .max_result_bytes
                .zip(result_bytes)
                .is_some_and(|(max, size)| size > max)
            || self
                .max_outbox_jobs
                .is_some_and(|max| usage.outbox_jobs > max)
            || self
                .max_outbox_bytes
                .is_some_and(|max| usage.outbox_bytes > max)
        {
            return Err(Error::QuotaExceeded);
        }
        Ok(())
    }
}

impl ManagedStore {
    pub async fn require_operator(&self) -> Result<(), Error> {
        self.require_initialized()?;
        let allowed: bool = self
            .client
            .query_one(
                "SELECT has_schema_privilege(current_user,$1,'CREATE')",
                &[&self.schema.namespace],
            )
            .await?
            .get(0);
        if allowed {
            Ok(())
        } else {
            Err(Error::Forbidden)
        }
    }
    /// Trusted operator only. Lowering a quota below current usage fails without
    /// changing it. Runtime roles have no write permission on the quota table.
    pub async fn set_scope_quota(&mut self, scope: Uuid, quota: ScopeQuota) -> Result<(), Error> {
        self.require_initialized()?;
        quota.validate()?;
        let tx = self.client.transaction().await?;
        lock_scope(&tx, &self.schema, scope).await?;
        quota.check(usage(&tx, &self.schema, scope).await?, None)?;
        tx.execute(&format!("INSERT INTO {} VALUES ($1,$2,$3,$4,$5,$6,$7) ON CONFLICT(scope) DO UPDATE SET max_rows=$2,max_data_bytes=$3,max_receipt_bytes=$4,max_result_bytes=$5,max_outbox_jobs=$6,max_outbox_bytes=$7", self.schema.relation("quotas")),
            &[&scope, &quota.max_rows, &quota.max_data_bytes, &quota.max_receipt_bytes, &quota.max_result_bytes, &quota.max_outbox_jobs, &quota.max_outbox_bytes]).await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn scope_usage(&mut self, scope: Uuid) -> Result<Usage, Error> {
        self.require_initialized()?;
        let tx = self
            .client
            .build_transaction()
            .isolation_level(tokio_postgres::IsolationLevel::RepeatableRead)
            .read_only(true)
            .start()
            .await?;
        if tx
            .query_opt(
                &format!(
                    "SELECT id FROM {} WHERE id=$1",
                    self.schema.relation("scopes")
                ),
                &[&scope],
            )
            .await?
            .is_none()
        {
            return Err(Error::NotFound);
        }
        let value = usage(&tx, &self.schema, scope).await?;
        tx.commit().await?;
        Ok(value)
    }

    /// Read-only operator diagnostics in one repeatable-read snapshot. This scans
    /// the scope's data; poll at an operator-selected cadence, not on every request.
    pub async fn scope_status(&mut self, scope: Uuid) -> Result<ScopeStatus, Error> {
        self.require_operator().await?;
        let tx = self
            .client
            .build_transaction()
            .isolation_level(tokio_postgres::IsolationLevel::RepeatableRead)
            .read_only(true)
            .start()
            .await?;
        let sequence: i64 = tx
            .query_opt(
                &format!(
                    "SELECT sequence FROM {} WHERE id=$1",
                    self.schema.relation("scopes")
                ),
                &[&scope],
            )
            .await?
            .ok_or(Error::NotFound)?
            .get(0);
        let source: Uuid = tx
            .query_one(
                &format!(
                    "SELECT source FROM {} WHERE singleton",
                    self.schema.relation("metadata")
                ),
                &[],
            )
            .await?
            .get(0);
        let quota = quota(&tx, &self.schema, scope).await?;
        let usage = usage(&tx, &self.schema, scope).await?;
        let receipts = tx.query_one(&format!("SELECT count(*) FILTER (WHERE NOT expired), count(*) FILTER (WHERE expired), GREATEST(0,EXTRACT(EPOCH FROM (CURRENT_TIMESTAMP-min(created_at) FILTER (WHERE NOT expired)))*1000)::bigint FROM {} WHERE scope=$1", self.schema.relation("receipts")), &[&scope]).await?;
        // GREATEST ignores NULL in PostgreSQL, so explicitly preserve an absent age.
        let retained: i64 = receipts.get(0);
        let receipts = ReceiptStatus {
            retained,
            expired: receipts.get(1),
            oldest_retained_age_ms: (retained > 0).then(|| receipts.get(2)),
        };
        let jobs = tx.query_one(&format!("WITH jobs AS (SELECT *, (source=$2 AND NOT failed AND attempts<max_attempts AND (lease_until IS NULL OR lease_until<=CURRENT_TIMESTAMP)) AS available FROM {} WHERE scope=$1) SELECT count(*) FILTER (WHERE available AND available_at<=CURRENT_TIMESTAMP), count(*) FILTER (WHERE available AND available_at>CURRENT_TIMESTAMP), count(*) FILTER (WHERE source=$2 AND NOT failed AND lease_until>CURRENT_TIMESTAMP), count(*) FILTER (WHERE source=$2 AND (failed OR (attempts>=max_attempts AND (lease_until IS NULL OR lease_until<=CURRENT_TIMESTAMP)))), count(*) FILTER (WHERE source<>$2), GREATEST(0,EXTRACT(EPOCH FROM (CURRENT_TIMESTAMP-min(available_at) FILTER (WHERE available AND available_at<=CURRENT_TIMESTAMP)))*1000)::bigint FROM jobs", self.schema.relation("outbox")), &[&scope, &source]).await?;
        let ready: i64 = jobs.get(0);
        let outbox = OutboxStatus {
            ready,
            delayed: jobs.get(1),
            leased: jobs.get(2),
            dead: jobs.get(3),
            old_source: jobs.get(4),
            oldest_ready_age_ms: (ready > 0).then(|| jobs.get(5)),
        };
        tx.commit().await?;
        Ok(ScopeStatus {
            source,
            sequence,
            quota,
            usage,
            receipts,
            outbox,
        })
    }

    /// Explicit, bounded operator retention. Removes results while keeping
    /// fixed-size input fingerprints and permanent request markers.
    /// Old retries return ReceiptExpired, never None.
    /// No automatic cleanup or request-marker deletion is performed.
    pub async fn compact_receipts(
        &mut self,
        scope: Uuid,
        expected_source: Uuid,
        minimum_age: Duration,
        limit: u16,
    ) -> Result<u64, Error> {
        self.require_operator().await?;
        if minimum_age.as_millis() == 0
            || minimum_age > Duration::from_secs(100 * 365 * 24 * 60 * 60)
            || !(1..=1000).contains(&limit)
        {
            return Err(Error::InvalidInput(
                "receipt retention needs an age of 1ms..100 years and a batch limit of 1..1000"
                    .into(),
            ));
        }
        let age_ms = minimum_age.as_millis() as i64;
        let tx = self.client.transaction().await?;
        lock_scope(&tx, &self.schema, scope).await?;
        let source: Uuid = tx
            .query_one(
                &format!(
                    "SELECT source FROM {} WHERE singleton",
                    self.schema.relation("metadata")
                ),
                &[],
            )
            .await?
            .get(0);
        if source != expected_source {
            return Err(Error::SourceMismatch);
        }
        let relation = self.schema.relation("receipts");
        let changed = tx.execute(&format!("WITH selected AS (SELECT scope,principal,request_id FROM {relation} WHERE scope=$1 AND NOT expired AND created_at <= clock_timestamp()-($2::bigint * interval '1 millisecond') ORDER BY created_at,principal,request_id LIMIT $3 FOR UPDATE SKIP LOCKED) UPDATE {relation} r SET receipt=jsonb_set(r.receipt,'{{result}}','null'::jsonb),expired=true FROM selected s WHERE (r.scope,r.principal,r.request_id)=(s.scope,s.principal,s.request_id)"), &[&scope, &age_ms, &i64::from(limit)]).await?;
        tx.commit().await.map_err(super::store::commit_error)?;
        Ok(changed)
    }

    /// Restore/maintenance fence. All managed tables are locked before source
    /// rotation, so no pre-rotation mutation can commit after this returns.
    /// Existing receipts keep their original source; never rebind their fences.
    pub async fn rotate_source(&mut self, expected_source: Uuid) -> Result<Uuid, Error> {
        self.require_initialized()?;
        let tx = self.client.transaction().await?;
        let mut relations = vec![
            self.schema.relation("scopes"),
            self.schema.relation("metadata"),
            self.schema.relation("receipts"),
            self.schema.relation("tickets"),
            self.schema.relation("admissions"),
            self.schema.relation("quotas"),
            self.schema.relation("outbox"),
        ];
        relations.extend(
            self.schema
                .contract
                .tables()
                .iter()
                .map(|t| self.schema.table_relation(t)),
        );
        tx.batch_execute(&format!(
            "LOCK TABLE {} IN ACCESS EXCLUSIVE MODE",
            relations.join(",")
        ))
        .await?;
        let source = Uuid::now_v7();
        let changed = tx
            .execute(
                &format!(
                    "UPDATE {} SET source=$2 WHERE singleton AND source=$1",
                    self.schema.relation("metadata")
                ),
                &[&expected_source, &source],
            )
            .await?;
        if changed != 1 {
            return Err(Error::SourceMismatch);
        }
        tx.commit().await?;
        Ok(source)
    }

    /// Run with an operator connection that can create/own roles. No passwords
    /// or browser grants are created here. Bind existing logins separately.
    pub async fn provision_roles(&mut self, roles: &Roles) -> Result<(), Error> {
        self.require_initialized()?;
        let names = [&roles.owner, &roles.runtime, &roles.backup, &roles.worker];
        if names
            .iter()
            .enumerate()
            .any(|(index, name)| names[index + 1..].contains(name))
        {
            return Err(Error::InvalidInput(
                "operator, runtime, backup and worker roles must differ".into(),
            ));
        }
        let quoted = names
            .iter()
            .map(|n| identifier(n))
            .collect::<Result<Vec<_>, _>>()?;
        let tx = self.client.transaction().await?;
        let mut ordered = names;
        ordered.sort();
        for name in ordered {
            tx.execute(
                "SELECT pg_advisory_xact_lock(hashtextextended($1,1))",
                &[&name],
            )
            .await?;
            if let Some(row) = tx.query_opt("SELECT rolcanlogin OR rolsuper OR rolcreatedb OR rolcreaterole OR rolreplication OR rolbypassrls OR EXISTS(SELECT 1 FROM pg_auth_members WHERE member=pg_roles.oid) FROM pg_roles WHERE rolname=$1", &[&name]).await? {
                if row.get::<_, bool>(0) { return Err(Error::InvalidInput("existing managed role is privileged or inherits another role".into())); }
            } else {
                tx.batch_execute(&format!("CREATE ROLE {} NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS", identifier(name)?)).await?;
            }
        }
        // Refuse the runtime/backup groups owning objects anywhere in this DB.
        for name in [&roles.runtime, &roles.backup, &roles.worker] {
            let owns: bool = tx.query_one("SELECT EXISTS(SELECT 1 FROM pg_class c JOIN pg_roles r ON c.relowner=r.oid WHERE r.rolname=$1) OR EXISTS(SELECT 1 FROM pg_namespace n JOIN pg_roles r ON n.nspowner=r.oid WHERE r.rolname=$1)", &[&name]).await?.get(0);
            if owns {
                return Err(Error::InvalidInput(
                    "runtime/backup/worker roles must not own database objects".into(),
                ));
            }
        }
        let (owner, runtime, backup, worker) = (&quoted[0], &quoted[1], &quoted[2], &quoted[3]);
        let namespace = identifier(&self.schema.namespace)?;
        tx.batch_execute(&format!("REVOKE ALL ON SCHEMA {namespace} FROM PUBLIC,{runtime},{backup},{worker}; ALTER SCHEMA {namespace} OWNER TO {owner}; GRANT USAGE ON SCHEMA {namespace} TO {runtime},{backup},{worker};")).await?;
        let mut relations = vec![
            self.schema.relation("scopes"),
            self.schema.relation("metadata"),
            self.schema.relation("receipts"),
            self.schema.relation("tickets"),
            self.schema.relation("admissions"),
            self.schema.relation("quotas"),
            self.schema.relation("outbox"),
        ];
        relations.extend(
            self.schema
                .contract
                .tables()
                .iter()
                .map(|t| self.schema.table_relation(t)),
        );
        for relation in &relations {
            let columns = tx.query("SELECT attname FROM pg_attribute WHERE attrelid=to_regclass($1) AND attnum > 0 AND NOT attisdropped", &[&relation]).await?.into_iter().map(|r| identifier(&r.get::<_, String>(0))).collect::<Result<Vec<_>, _>>()?.join(",");
            tx.batch_execute(&format!(
                "REVOKE ALL ({columns}) ON {relation} FROM PUBLIC,{runtime},{backup},{worker};"
            ))
            .await?;
            tx.batch_execute(&format!("REVOKE ALL ON {relation} FROM PUBLIC,{runtime},{backup},{worker}; ALTER TABLE {relation} OWNER TO {owner}; GRANT SELECT ON {relation} TO {runtime},{backup};")).await?;
        }
        tx.batch_execute(&format!(
            "GRANT UPDATE(sequence) ON {} TO {runtime}; GRANT INSERT ON {} TO {runtime}; GRANT INSERT(id,source,scope,principal,authorization_scope,request_id,operation,name,payload,max_attempts,available_at) ON {} TO {runtime}; GRANT SELECT ON {},{} TO {worker}; GRANT UPDATE(attempts,available_at,lease_token,lease_until,failed,last_error),DELETE ON {} TO {worker};",
            self.schema.relation("scopes"),
            self.schema.relation("receipts"),
            self.schema.relation("outbox"),
            self.schema.relation("metadata"),
            self.schema.relation("outbox"),
            self.schema.relation("outbox"),
        ))
        .await?;
        tx.batch_execute(&format!("GRANT INSERT,UPDATE,DELETE ON {},{} TO {runtime}; GRANT UPDATE(receipt,expired),DELETE ON {} TO {runtime};", self.schema.relation("tickets"),self.schema.relation("admissions"),self.schema.relation("receipts"))).await?;
        for table in self.schema.contract.tables() {
            tx.batch_execute(&format!(
                "GRANT INSERT,UPDATE,DELETE ON {} TO {runtime};",
                self.schema.table_relation(table)
            ))
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Membership is explicit. A login granted the runtime/backup group must
    /// not already inherit an operator role or have administrative attributes.
    pub async fn grant_role(&mut self, group: &str, login: &str) -> Result<(), Error> {
        let group_ident = identifier(group)?;
        let login_ident = identifier(login)?;
        let unsafe_login: bool = self.client.query_opt("SELECT rolsuper OR rolcreatedb OR rolcreaterole OR rolreplication OR rolbypassrls OR EXISTS(SELECT 1 FROM pg_auth_members WHERE member=pg_roles.oid AND roleid<>(SELECT oid FROM pg_roles WHERE rolname=$2)) OR EXISTS(SELECT 1 FROM pg_class WHERE relowner=pg_roles.oid) OR EXISTS(SELECT 1 FROM pg_namespace WHERE nspowner=pg_roles.oid) FROM pg_roles WHERE rolname=$1 AND rolcanlogin", &[&login, &group]).await?.ok_or_else(|| Error::InvalidInput("login role does not exist".into()))?.get(0);
        if unsafe_login {
            return Err(Error::InvalidInput(
                "use an unprivileged dedicated login".into(),
            ));
        }
        self.client
            .batch_execute(&format!(
                "GRANT {group_ident} TO {login_ident} WITH INHERIT TRUE, SET TRUE, ADMIN FALSE"
            ))
            .await?;
        Ok(())
    }
}

async fn lock_scope(
    tx: &Transaction<'_>,
    schema: &ManagedSchema,
    scope: Uuid,
) -> Result<(), Error> {
    tx.query_opt(
        &format!(
            "SELECT id FROM {} WHERE id=$1 FOR UPDATE",
            schema.relation("scopes")
        ),
        &[&scope],
    )
    .await?
    .ok_or(Error::NotFound)?;
    Ok(())
}
pub(super) async fn usage(
    tx: &Transaction<'_>,
    schema: &ManagedSchema,
    scope: Uuid,
) -> Result<Usage, Error> {
    let mut result = Usage::default();
    for table in schema.contract.tables() {
        let row = tx.query_one(&format!("SELECT count(*)::bigint,COALESCE(sum(pg_column_size(t)),0)::bigint FROM {} t WHERE _scope=$1", schema.table_relation(table)), &[&scope]).await?;
        result.rows = result
            .rows
            .checked_add(row.get(0))
            .ok_or(Error::CorruptData)?;
        result.data_bytes = result
            .data_bytes
            .checked_add(row.get(1))
            .ok_or(Error::CorruptData)?;
    }
    let outbox = tx.query_one(&format!("SELECT count(*)::bigint,COALESCE(sum(octet_length(payload::text)+octet_length(name)),0)::bigint FROM {} t WHERE scope=$1", schema.relation("outbox")), &[&scope]).await?;
    result.outbox_jobs = outbox.get(0);
    result.outbox_bytes = outbox.get(1);
    result.receipt_bytes = tx
        .query_one(
            &format!(
                "SELECT COALESCE(sum(pg_column_size(t)),0)::bigint FROM {} t WHERE scope=$1",
                schema.relation("receipts")
            ),
            &[&scope],
        )
        .await?
        .get(0);
    let admission_bytes: i64 = tx
        .query_one(
            &format!(
                "SELECT COALESCE(sum(pg_column_size(a)+CASE WHEN status='pending' THEN {} ELSE 0 END),0)::bigint FROM {} a WHERE scope=$1",
                hulladb_protocol::MAX_APPLICATION_ERROR_BYTES + 128,
                schema.relation("admissions")
            ),
            &[&scope],
        )
        .await?
        .get(0);
    let ticket_bytes: i64 = tx
        .query_one(
            &format!(
                "SELECT COALESCE(sum(pg_column_size(t)),0)::bigint FROM {} t WHERE scope=$1",
                schema.relation("tickets")
            ),
            &[&scope],
        )
        .await?
        .get(0);
    result.receipt_bytes = result
        .receipt_bytes
        .checked_add(admission_bytes)
        .and_then(|bytes| bytes.checked_add(ticket_bytes))
        .ok_or(Error::CorruptData)?;
    Ok(result)
}
pub(super) async fn enforce_quota(
    tx: &Transaction<'_>,
    schema: &ManagedSchema,
    scope: Uuid,
    result_bytes: i64,
) -> Result<(), Error> {
    let quota = quota(tx, schema, scope).await?;
    if quota != ScopeQuota::default() {
        quota.check(usage(tx, schema, scope).await?, Some(result_bytes))?;
    }
    Ok(())
}
async fn quota(
    tx: &Transaction<'_>,
    schema: &ManagedSchema,
    scope: Uuid,
) -> Result<ScopeQuota, Error> {
    let row = tx.query_opt(&format!("SELECT max_rows,max_data_bytes,max_receipt_bytes,max_result_bytes,max_outbox_jobs,max_outbox_bytes FROM {} WHERE scope=$1", schema.relation("quotas")), &[&scope]).await?;
    Ok(row
        .map(|row| ScopeQuota {
            max_rows: row.get(0),
            max_data_bytes: row.get(1),
            max_receipt_bytes: row.get(2),
            max_result_bytes: row.get(3),
            max_outbox_jobs: row.get(4),
            max_outbox_bytes: row.get(5),
        })
        .unwrap_or_default())
}
