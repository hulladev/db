use std::time::Duration;

use serde_json::Value as Json;
use tokio::task::JoinHandle;
use tokio_postgres::{
    Client, GenericClient, IsolationLevel, NoTls, Row, Transaction, types::ToSql,
};
use uuid::Uuid;

use crate::{
    Change, Command, Error, MAX_PAGE, Page, QueryRevision, QuerySnapshot, Receipt, Session, Task,
    TaskQuery, TaskSummary,
};

pub struct Store {
    client: Client,
    connection: JoinHandle<Result<(), tokio_postgres::Error>>,
}

impl Drop for Store {
    fn drop(&mut self) {
        self.connection.abort();
    }
}

impl Store {
    /// Local development connection. Production TLS and pooling are later work.
    pub async fn connect(url: &str) -> Result<Self, Error> {
        let mut config: tokio_postgres::Config = url.parse()?;
        config.connect_timeout(Duration::from_secs(5));
        let (client, connection) = config.connect(NoTls).await?;
        let connection = tokio::spawn(connection);
        let store = Self { client, connection };
        store
            .client
            .batch_execute("SET statement_timeout = '5s'; SET lock_timeout = '5s';")
            .await?;
        Ok(store)
    }

    /// Install the fixed development schema. This is not a migration compiler.
    pub async fn initialize(&mut self) -> Result<(), Error> {
        let tx = self.client.transaction().await?;
        tx.execute("SELECT pg_advisory_xact_lock(782116234)", &[])
            .await?;
        tx.batch_execute(include_str!("schema.sql")).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Trusted fixture administration, never a client-facing operation.
    pub async fn create_workspace(&mut self, principal: Uuid) -> Result<Session, Error> {
        let session = Session {
            workspace: Uuid::now_v7(),
            principal,
        };
        let tx = self.client.transaction().await?;
        tx.execute(
            "INSERT INTO _hulladb_task_probe.workspaces(id) VALUES ($1)",
            &[&session.workspace],
        )
        .await?;
        tx.execute(
            "INSERT INTO _hulladb_task_probe.members VALUES ($1,$2,true)",
            &[&session.workspace, &principal],
        )
        .await?;
        tx.commit().await?;
        Ok(session)
    }

    /// Trusted fixture administration. None removes membership; Some(false) grants read-only access.
    pub async fn set_member(
        &mut self,
        workspace: Uuid,
        principal: Uuid,
        can_write: Option<bool>,
    ) -> Result<(), Error> {
        let tx = self.client.transaction().await?;
        lock_workspace(&tx, workspace).await?;
        if let Some(can_write) = can_write {
            tx.execute(
                "INSERT INTO _hulladb_task_probe.members VALUES ($1,$2,$3) ON CONFLICT (workspace,principal) DO UPDATE SET can_write=$3",
                &[&workspace, &principal, &can_write],
            ).await?;
        } else {
            tx.execute(
                "DELETE FROM _hulladb_task_probe.members WHERE workspace=$1 AND principal=$2",
                &[&workspace, &principal],
            )
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn execute(
        &mut self,
        session: Session,
        request_id: Uuid,
        command: Command,
    ) -> Result<Receipt, Error> {
        command.validate()?;
        let payload = serde_json::to_value(&command).map_err(|_| Error::CorruptData)?;
        // All managed mutations in one workspace share this lock. READ COMMITTED
        // observes the preceding holder's commit after waiting, before any reads.
        let tx = self.client.transaction().await?;
        let sequence = lock_workspace(&tx, session.workspace).await?;
        authorize(&tx, session, true).await?;
        if let Some(row) = tx.query_opt(
            "SELECT payload,receipt FROM _hulladb_task_probe.receipts WHERE workspace=$1 AND principal=$2 AND request_id=$3",
            &[&session.workspace, &session.principal, &request_id],
        ).await? {
            let previous: Json = row.try_get(0)?;
            if previous != payload { return Err(Error::RequestIdReused); }
            let receipt = decode(row.try_get(1)?)?;
            tx.commit().await?;
            return Ok(receipt);
        }
        let change = mutate(&tx, session.workspace, command).await?;
        let next = sequence.checked_add(1).ok_or(Error::CorruptData)?;
        let receipt = Receipt {
            request_id,
            revision: QueryRevision::new(session.workspace, next),
            change,
        };
        let encoded = serde_json::to_value(&receipt).map_err(|_| Error::CorruptData)?;
        tx.execute(
            "UPDATE _hulladb_task_probe.workspaces SET sequence=$2 WHERE id=$1",
            &[&session.workspace, &next],
        )
        .await?;
        tx.execute(
            "INSERT INTO _hulladb_task_probe.receipts VALUES ($1,$2,$3,$4,$5)",
            &[
                &session.workspace,
                &session.principal,
                &request_id,
                &payload,
                &encoded,
            ],
        )
        .await?;
        tx.commit().await.map_err(|_| Error::CommitOutcomeUnknown)?;
        Ok(receipt)
    }

    /// Recover a submitted request without resubmission. Current read authorization is required.
    pub async fn receipt(
        &mut self,
        session: Session,
        request_id: Uuid,
    ) -> Result<Option<Receipt>, Error> {
        let tx = self
            .client
            .build_transaction()
            .isolation_level(IsolationLevel::RepeatableRead)
            .read_only(true)
            .start()
            .await?;
        authorize(&tx, session, false).await?;
        let row = tx.query_opt("SELECT receipt FROM _hulladb_task_probe.receipts WHERE workspace=$1 AND principal=$2 AND request_id=$3",
            &[&session.workspace, &session.principal, &request_id]).await?;
        let receipt = row
            .map(|row| {
                row.try_get::<_, Json>(0)
                    .map_err(Error::from)
                    .and_then(decode)
            })
            .transpose()?;
        tx.commit().await?;
        Ok(receipt)
    }

    /// Indexed keyset page. Projection and permission are evaluated in one read snapshot.
    pub async fn list_tasks(
        &mut self,
        session: Session,
        completed: Option<bool>,
        after: Option<Uuid>,
        limit: u16,
    ) -> Result<Page, Error> {
        validate_limit(limit)?;
        let tx = self
            .client
            .build_transaction()
            .isolation_level(IsolationLevel::RepeatableRead)
            .read_only(true)
            .start()
            .await?;
        authorize(&tx, session, false).await?;
        let fetch = i64::from(limit) + 1;
        let (sql, params) = task_select(
            "id,title,completed,version",
            &session.workspace,
            &completed,
            &after,
            &fetch,
        );
        let rows = tx.query(&sql, &params).await?;
        let mut tasks = rows.iter().map(task).collect::<Result<Vec<_>, _>>()?;
        let has_more = tasks.len() > usize::from(limit);
        tasks.truncate(usize::from(limit));
        let next = if has_more {
            tasks.last().map(|task| task.id)
        } else {
            None
        };
        tx.commit().await?;
        Ok(Page { tasks, next })
    }

    /// Explicit task summaries and freshness revision from one snapshot, complete for the bounded query.
    pub async fn query_snapshot(
        &mut self,
        session: Session,
        query: TaskQuery,
    ) -> Result<QuerySnapshot, Error> {
        query.validate()?;
        let tx = self
            .client
            .build_transaction()
            .isolation_level(IsolationLevel::RepeatableRead)
            .read_only(true)
            .start()
            .await?;
        authorize(&tx, session, false).await?;
        let sequence: i64 = tx
            .query_one(
                "SELECT sequence FROM _hulladb_task_probe.workspaces WHERE id=$1",
                &[&session.workspace],
            )
            .await?
            .try_get(0)?;
        // The projection is explicit at the storage boundary; version and future
        // table columns are never fetched and stripped after delivery.
        let limit = i64::from(query.limit);
        let (sql, params) = task_select(
            "id,title,completed",
            &session.workspace,
            &query.completed,
            &query.after,
            &limit,
        );
        let rows = tx.query(&sql, &params).await?;
        let mut tasks = Vec::with_capacity(rows.len());
        for row in rows {
            let task = TaskSummary {
                id: row.try_get("id")?,
                title: row.try_get("title")?,
                completed: row.try_get("completed")?,
            };
            task.validate()?;
            tasks.push(task);
        }
        tx.commit().await?;
        Ok(QuerySnapshot {
            session,
            query,
            revision: QueryRevision::new(session.workspace, sequence),
            tasks,
        })
    }

    /// Bulk bootstrap for query benchmarks only, before any tasks or mutation receipts exist.
    pub async fn seed_fixture(&mut self, session: Session, count: i32) -> Result<(), Error> {
        if !(0..=1_000_000).contains(&count) {
            return Err(Error::InvalidInput("fixture size exceeds limit"));
        }
        let tx = self.client.transaction().await?;
        let sequence = lock_workspace(&tx, session.workspace).await?;
        authorize(&tx, session, true).await?;
        let exists: bool = tx
            .query_one(
                "SELECT EXISTS(SELECT 1 FROM _hulladb_task_probe.tasks WHERE workspace=$1)",
                &[&session.workspace],
            )
            .await?
            .try_get(0)?;
        if sequence != 0 || exists {
            return Err(Error::InvalidInput("fixture workspace must be empty"));
        }
        tx.execute("INSERT INTO _hulladb_task_probe.tasks SELECT $1,md5(i::text)::uuid,'task '||i,(i%2=0),1 FROM generate_series(1,$2::integer) i",
            &[&session.workspace, &count]).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Trusted benchmark diagnostics for the fixed status/keyset query shape.
    pub async fn fixture_diagnostics(&self, workspace: Uuid) -> Result<Json, Error> {
        let version: String = self
            .client
            .query_one("SHOW server_version", &[])
            .await?
            .try_get(0)?;
        let settings = self.client.query_one("SELECT json_build_object('wal_level',current_setting('wal_level'),'synchronous_commit',current_setting('synchronous_commit'),'fsync',current_setting('fsync'),'full_page_writes',current_setting('full_page_writes'),'max_slot_wal_keep_size',current_setting('max_slot_wal_keep_size'))", &[]).await?.try_get::<_, Json>(0)?;
        let after = Uuid::from_u128(1 << 127);
        let plan: Json = self.client.query_one(
            "EXPLAIN (ANALYZE, BUFFERS, FORMAT JSON) SELECT id,title,completed,version FROM _hulladb_task_probe.tasks WHERE workspace=$1 AND completed=false AND id>$2 ORDER BY id LIMIT 51",
            &[&workspace, &after],
        ).await?.try_get(0)?;
        Ok(
            serde_json::json!({"server_version": version, "settings": settings, "status_keyset_plan": plan}),
        )
    }

    /// Remove only the caller-created development fixture and its owned rows.
    pub async fn delete_fixture_workspace(&mut self, workspace: Uuid) -> Result<(), Error> {
        let tx = self.client.transaction().await?;
        lock_workspace(&tx, workspace).await?;
        for table in ["receipts", "tasks", "members"] {
            tx.execute(
                &format!("DELETE FROM _hulladb_task_probe.{table} WHERE workspace=$1"),
                &[&workspace],
            )
            .await?;
        }
        tx.execute(
            "DELETE FROM _hulladb_task_probe.workspaces WHERE id=$1",
            &[&workspace],
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }
}

// Column lists come only from fixed call sites; every query argument is bound.
// Sharing predicates/order avoids drift between full-row pages and summary coverage.
fn task_select<'a>(
    columns: &'static str,
    workspace: &'a Uuid,
    completed: &'a Option<bool>,
    after: &'a Option<Uuid>,
    fetch: &'a i64,
) -> (String, Vec<&'a (dyn ToSql + Sync)>) {
    let mut sql = format!("SELECT {columns} FROM _hulladb_task_probe.tasks WHERE workspace=$1");
    let mut params: Vec<&(dyn ToSql + Sync)> = vec![workspace];
    if let Some(completed) = completed {
        params.push(completed);
        sql.push_str(&format!(" AND completed=${}", params.len()));
    }
    if let Some(after) = after {
        params.push(after);
        sql.push_str(&format!(" AND id>${}", params.len()));
    }
    params.push(fetch);
    sql.push_str(&format!(" ORDER BY id LIMIT ${}", params.len()));
    (sql, params)
}

fn validate_limit(limit: u16) -> Result<(), Error> {
    if limit == 0 || limit > MAX_PAGE {
        return Err(Error::InvalidInput("page limit must be 1..100"));
    }
    Ok(())
}

async fn lock_workspace(tx: &Transaction<'_>, workspace: Uuid) -> Result<i64, Error> {
    let row = tx
        .query_opt(
            "SELECT sequence FROM _hulladb_task_probe.workspaces WHERE id=$1 FOR UPDATE",
            &[&workspace],
        )
        .await?;
    row.ok_or(Error::Forbidden)?.try_get(0).map_err(Error::from)
}

async fn authorize(
    client: &impl GenericClient,
    session: Session,
    write: bool,
) -> Result<(), Error> {
    let row = client
        .query_opt(
            "SELECT can_write FROM _hulladb_task_probe.members WHERE workspace=$1 AND principal=$2",
            &[&session.workspace, &session.principal],
        )
        .await?;
    let Some(row) = row else {
        return Err(Error::Forbidden);
    };
    if write && !row.try_get::<_, bool>(0)? {
        return Err(Error::Forbidden);
    }
    Ok(())
}

fn task(row: &Row) -> Result<Task, Error> {
    let task = Task {
        id: row.try_get("id")?,
        title: row.try_get("title")?,
        completed: row.try_get("completed")?,
        version: row.try_get("version")?,
    };
    task.validate()?;
    Ok(task)
}

fn decode<T: serde::de::DeserializeOwned>(value: Json) -> Result<T, Error> {
    serde_json::from_value(value).map_err(|_| Error::CorruptData)
}

async fn mutate(tx: &Transaction<'_>, workspace: Uuid, command: Command) -> Result<Change, Error> {
    match command {
        Command::Create {
            id,
            title,
            completed,
        } => {
            let row = tx.query_opt("INSERT INTO _hulladb_task_probe.tasks VALUES ($1,$2,$3,$4,1) ON CONFLICT (workspace,id) DO NOTHING RETURNING id,title,completed,version",
                &[&workspace, &id, &title, &completed]).await?.ok_or(Error::Conflict)?;
            Ok(Change::Upsert(task(&row)?))
        }
        Command::Patch {
            id,
            expected_version,
            title,
            completed,
        } => {
            let row = tx.query_opt("UPDATE _hulladb_task_probe.tasks SET title=COALESCE($4,title),completed=COALESCE($5,completed),version=version+1 WHERE workspace=$1 AND id=$2 AND version=$3 RETURNING id,title,completed,version",
                &[&workspace, &id, &expected_version, &title, &completed]).await?;
            match row {
                Some(row) => Ok(Change::Upsert(task(&row)?)),
                None => Err(missing_or_conflict(tx, workspace, id).await?),
            }
        }
        Command::Delete {
            id,
            expected_version,
        } => {
            let row = tx.query_opt("DELETE FROM _hulladb_task_probe.tasks WHERE workspace=$1 AND id=$2 AND version=$3 RETURNING id", &[&workspace, &id, &expected_version]).await?;
            if row.is_none() {
                return Err(missing_or_conflict(tx, workspace, id).await?);
            }
            Ok(Change::Delete(id))
        }
    }
}

async fn missing_or_conflict(
    tx: &Transaction<'_>,
    workspace: Uuid,
    id: Uuid,
) -> Result<Error, Error> {
    let exists = tx
        .query_opt(
            "SELECT 1 FROM _hulladb_task_probe.tasks WHERE workspace=$1 AND id=$2",
            &[&workspace, &id],
        )
        .await?
        .is_some();
    Ok(if exists {
        Error::Conflict
    } else {
        Error::NotFound
    })
}
