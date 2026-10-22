use super::{CompletedMutation, Life, Partition, Session};
use crate::{ClientFailure, Error, MutationIntent, Receipt, Snapshot};
use rusqlite::{Connection, OptionalExtension, params};
use std::{
    path::Path,
    sync::{Arc, Mutex},
};
use uuid::Uuid;

const TABLES: [(&str, &str); 4] = [
    (
        "function_partitions",
        "CREATE TABLE function_partitions (partition TEXT PRIMARY KEY, source TEXT NOT NULL)",
    ),
    (
        "function_results",
        "CREATE TABLE function_results (partition TEXT NOT NULL, query TEXT NOT NULL, snapshot TEXT NOT NULL, PRIMARY KEY(partition,query))",
    ),
    (
        "function_intents",
        "CREATE TABLE function_intents (partition TEXT NOT NULL, request TEXT NOT NULL, source TEXT NOT NULL, operation TEXT NOT NULL, input TEXT NOT NULL, ticket TEXT NOT NULL, PRIMARY KEY(partition,request))",
    ),
    (
        "function_receipts",
        "CREATE TABLE function_receipts (partition TEXT NOT NULL, request TEXT NOT NULL, intent TEXT NOT NULL, receipt TEXT NOT NULL, PRIMARY KEY(partition,request))",
    ),
];
fn storage() -> Error {
    Error::Client(ClientFailure::Storage)
}
// Disposable snapshots share one store-wide budget; recovery records are never
// evicted. Row IDs order the most recently refreshed results without a migration.
fn prune_results(db: &Connection) -> Result<(), Error> {
    db.execute("DELETE FROM function_results WHERE rowid IN (
        SELECT rowid FROM (
            SELECT rowid, ROW_NUMBER() OVER (ORDER BY rowid DESC) AS position,
                SUM(length(CAST(partition AS BLOB)) + length(CAST(query AS BLOB)) + length(CAST(snapshot AS BLOB))) OVER (ORDER BY rowid DESC) AS bytes
            FROM function_results
        ) WHERE position > 128 OR bytes > 16777216
    )", []).map_err(|_| storage())?;
    Ok(())
}
fn check_source(db: &Connection, partition: &str, source: Uuid) -> Result<(), Error> {
    let stored: String = db
        .query_row(
            "SELECT source FROM function_partitions WHERE partition=?1",
            [partition],
            |row| row.get(0),
        )
        .map_err(|_| storage())?;
    if stored != source.to_string() {
        return Err(Error::SourceMismatch);
    }
    Ok(())
}

/// Shared SQLite result cache and write-ahead mutation intents. Credentials are never stored.
#[derive(Clone)]
pub struct RecoveryStore(Arc<Mutex<Connection>>);
impl RecoveryStore {
    pub async fn open(path: impl AsRef<Path>) -> Result<Self, Error> {
        let path = path.as_ref().to_owned();
        tokio::task::spawn_blocking(move || {
            let mut connection = Connection::open(path).map_err(|_| storage())?;
            connection.busy_timeout(std::time::Duration::from_secs(5)).map_err(|_| storage())?;
            connection.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;").map_err(|_| storage())?;
            let mode:String=connection.query_row("PRAGMA journal_mode",[],|row|row.get(0)).map_err(|_|storage())?;
            if mode != "wal" {return Err(storage());}
            let tx = connection.transaction().map_err(|_| storage())?;
            let existing:i64=tx.query_row("SELECT count(*) FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'",[],|row|row.get(0)).map_err(|_|storage())?;
            if existing==0 {
                for (_,ddl) in TABLES {tx.execute_batch(ddl).map_err(|_|storage())?;}
            } else {
                if existing!=TABLES.len() as i64 {return Err(storage());}
                for (name,expected) in TABLES {
                    let actual:String=tx.query_row("SELECT sql FROM sqlite_master WHERE type='table' AND name=?1",[name],|row|row.get(0)).map_err(|_|storage())?;
                    if actual!=expected {return Err(storage());}
                }
            }
            prune_results(&tx)?;
            tx.commit().map_err(|_| storage())?;
            Ok(Self(Arc::new(Mutex::new(connection))))
        }).await.map_err(|_| storage())?
    }
    async fn work<T: Send + 'static>(
        &self,
        action: impl FnOnce(&mut Connection) -> Result<T, Error> + Send + 'static,
    ) -> Result<T, Error> {
        let db = self.0.clone();
        tokio::task::spawn_blocking(move || {
            let mut db = db.lock().map_err(|_| storage())?;
            action(&mut db)
        })
        .await
        .map_err(|_| storage())?
    }
    pub(super) async fn bind(&self, partition: &Partition, source: Uuid) -> Result<(), Error> {
        let partition = partition.0.clone();
        self.work(move |db| {
            let tx = db.transaction().map_err(|_| storage())?;
            let stored: Option<String> = tx
                .query_row(
                    "SELECT source FROM function_partitions WHERE partition=?1",
                    [&partition],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|_| storage())?;
            if let Some(stored) = stored {
                if stored != source.to_string() {
                    return Err(Error::SourceMismatch);
                }
            } else {
                tx.execute(
                    "INSERT INTO function_partitions VALUES (?1,?2)",
                    params![partition, source.to_string()],
                )
                .map_err(|_| storage())?;
            }
            tx.commit().map_err(|_| storage())?;
            Ok(())
        })
        .await
    }
    pub async fn pending(&self, partition: &Partition) -> Result<Vec<MutationIntent>, Error> {
        let partition = partition.0.clone();
        self.work(move |db| {
            let mut query=db.prepare("SELECT request,source,operation,input FROM function_intents WHERE partition=?1 ORDER BY request").map_err(|_|storage())?;
            let rows=query.query_map([partition],|row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,String>(2)?,row.get::<_,String>(3)?))).map_err(|_|storage())?;
            rows.map(|row| {
                let (request,source,operation,input)=row.map_err(|_|storage())?;
                Ok(MutationIntent {
                    request_id:Uuid::parse_str(&request).map_err(|_|storage())?,
                    source:Uuid::parse_str(&source).map_err(|_|storage())?,
                    operation,
                    input:serde_json::from_str(&input).map_err(|_|storage())?,
                })
            }).collect()
        }).await
    }
    pub(super) async fn ticket(&self, partition: &Partition, request: Uuid) -> Result<Uuid, Error> {
        let partition = partition.0.clone();
        self.work(move |db| {
            let ticket: String = db
                .query_row(
                    "SELECT ticket FROM function_intents WHERE partition=?1 AND request=?2",
                    params![partition, request.to_string()],
                    |row| row.get(0),
                )
                .map_err(|_| storage())?;
            Uuid::parse_str(&ticket).map_err(|_| storage())
        })
        .await
    }
    /// Explicit reconciliation after preserving or resolving an intent. Retire its owners first.
    pub async fn discard_intent(&self, partition: &Partition, request: Uuid) -> Result<(), Error> {
        let partition = partition.0.clone();
        self.work(move |db| {
            db.execute(
                "DELETE FROM function_intents WHERE partition=?1 AND request=?2",
                params![partition, request.to_string()],
            )
            .map_err(|_| storage())?;
            Ok(())
        })
        .await
    }
    /// Source replacement is explicit and refused while unresolved writes remain.
    pub async fn reconcile_source(&self, partition: &Partition, source: Uuid) -> Result<(), Error> {
        let partition = partition.0.clone();
        self.work(move |db| {
            let tx=db.transaction().map_err(|_|storage())?;
            let pending:i64=tx.query_row("SELECT count(*) FROM function_intents WHERE partition=?1",[&partition],|row|row.get(0)).map_err(|_|storage())?;
            if pending!=0 {return Err(Error::CommitOutcomeUnknown);}
            tx.execute("DELETE FROM function_results WHERE partition=?1",[&partition]).map_err(|_|storage())?;
            tx.execute("INSERT INTO function_partitions VALUES (?1,?2) ON CONFLICT(partition) DO UPDATE SET source=excluded.source",params![partition,source.to_string()]).map_err(|_|storage())?;
            tx.commit().map_err(|_|storage())?;
            Ok(())
        }).await
    }
    pub(super) async fn put_intent(
        &self,
        partition: &Partition,
        intent: MutationIntent,
        ticket: Uuid,
        capacity: usize,
        life: Arc<Life>,
    ) -> Result<(), Error> {
        let partition = partition.0.clone();
        self.work(move |db| {
            let _active = life.active()?;
            let tx = db.transaction().map_err(|_| storage())?;
            check_source(&tx, &partition, intent.source)?;
            let count: i64 = tx
                .query_row(
                    "SELECT count(*) FROM function_intents WHERE partition=?1",
                    [&partition],
                    |row| row.get(0),
                )
                .map_err(|_| storage())?;
            if count as usize >= capacity {
                return Err(Error::Client(ClientFailure::Capacity));
            }
            tx.execute(
                "INSERT INTO function_intents VALUES (?1,?2,?3,?4,?5,?6)",
                params![
                    partition,
                    intent.request_id.to_string(),
                    intent.source.to_string(),
                    intent.operation,
                    serde_json::to_string(&intent.input).map_err(|_| storage())?,
                    ticket.to_string()
                ],
            )
            .map_err(|_| storage())?;
            tx.commit().map_err(|_| storage())?;
            Ok(())
        })
        .await
    }
    pub(super) async fn result(
        &self,
        partition: &Partition,
        key: String,
        session: Session,
        source: Uuid,
    ) -> Result<Option<Snapshot>, Error> {
        let partition = partition.0.clone();
        self.work(move |db| {
            check_source(db, &partition, source)?;
            let text: Option<String> = db
                .query_row(
                    "SELECT snapshot FROM function_results WHERE partition=?1 AND query=?2",
                    params![partition, key],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|_| storage())?;
            text.map(|text| {
                let result: Snapshot = serde_json::from_str(&text).map_err(|_| storage())?;
                if result.source != source
                    || result.revision.scope != session.scope
                    || result.principal != session.principal
                    || result.authorization_scope != session.authorization_scope
                    || result.revision.sequence < 0
                {
                    return Err(storage());
                }
                Ok(result)
            })
            .transpose()
        })
        .await
    }
    pub(super) async fn put_result(
        &self,
        partition: &Partition,
        key: String,
        snapshot: Snapshot,
        life: Arc<Life>,
    ) -> Result<Snapshot, Error> {
        let partition = partition.0.clone();
        self.work(move |db| {
            let _active = life.active()?;
            let tx = db.transaction().map_err(|_| storage())?;
            check_source(&tx, &partition, snapshot.source)?;
            let prior: Option<String> = tx
                .query_row(
                    "SELECT snapshot FROM function_results WHERE partition=?1 AND query=?2",
                    params![partition, key],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|_| storage())?;
            if let Some(prior) = prior {
                let prior: Snapshot = serde_json::from_str(&prior).map_err(|_| storage())?;
                if prior.source != snapshot.source {
                    return Err(Error::SourceMismatch);
                }
                if prior.principal != snapshot.principal
                    || prior.authorization_scope != snapshot.authorization_scope
                    || prior.revision.scope != snapshot.revision.scope
                    || prior.revision.sequence < 0
                {
                    return Err(storage());
                }
                if prior.revision.sequence > snapshot.revision.sequence {
                    return Ok(prior);
                }
            }
            let encoded = serde_json::to_string(&snapshot).map_err(|_| storage())?;
            if partition.len() + key.len() + encoded.len() <= 16 * 1024 * 1024 {
                tx.execute(
                    "INSERT OR REPLACE INTO function_results VALUES (?1,?2,?3)",
                    params![partition, key, encoded],
                )
                .map_err(|_| storage())?;
                prune_results(&tx)?;
            } else {
                tx.execute(
                    "DELETE FROM function_results WHERE partition=?1 AND query=?2",
                    params![partition, key],
                )
                .map_err(|_| storage())?;
            }
            tx.commit().map_err(|_| storage())?;
            Ok(snapshot)
        })
        .await
    }
    pub(super) async fn acknowledge(
        &self,
        partition: &Partition,
        intent: MutationIntent,
        receipt: Option<Receipt>,
        life: Arc<Life>,
    ) -> Result<(), Error> {
        let partition = partition.0.clone();
        self.work(move |db| {
            let _active=life.active()?;
            let tx=db.transaction().map_err(|_|storage())?;
            if let Some(receipt)=receipt {
                check_source(&tx,&partition,receipt.source)?;
                tx.execute("INSERT INTO function_receipts VALUES (?1,?2,?3,?4) ON CONFLICT(partition,request) DO UPDATE SET intent=excluded.intent,receipt=excluded.receipt",params![partition,intent.request_id.to_string(),serde_json::to_string(&intent).map_err(|_|storage())?,serde_json::to_string(&receipt).map_err(|_|storage())?]).map_err(|_|storage())?;
            }
            tx.execute("DELETE FROM function_intents WHERE partition=?1 AND request=?2",params![partition,intent.request_id.to_string()]).map_err(|_|storage())?;
            tx.commit().map_err(|_|storage())?;
            Ok(())
        }).await
    }
    /// Confirmed receipts survive cancellation at the acknowledgement boundary.
    pub async fn completed(&self, partition: &Partition) -> Result<Vec<CompletedMutation>, Error> {
        let partition = partition.0.clone();
        self.work(move |db| {
            let mut query=db.prepare("SELECT intent,receipt FROM function_receipts WHERE partition=?1 ORDER BY request").map_err(|_|storage())?;
            let rows=query.query_map([partition],|row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?))).map_err(|_|storage())?;
            rows.map(|row| {
                let (intent,receipt)=row.map_err(|_|storage())?;
                Ok(CompletedMutation{intent:serde_json::from_str(&intent).map_err(|_|storage())?,receipt:serde_json::from_str(&receipt).map_err(|_|storage())?})
            }).collect()
        }).await
    }
    /// Local receipt retention does not affect server deduplication.
    pub async fn forget_receipt(&self, partition: &Partition, request: Uuid) -> Result<(), Error> {
        let partition = partition.0.clone();
        self.work(move |db| {
            db.execute(
                "DELETE FROM function_receipts WHERE partition=?1 AND request=?2",
                params![partition, request.to_string()],
            )
            .map_err(|_| storage())?;
            Ok(())
        })
        .await
    }
    pub(super) async fn remove_result(
        &self,
        partition: &Partition,
        key: &str,
        life: Arc<Life>,
    ) -> Result<(), Error> {
        let partition = partition.0.clone();
        let key = key.to_owned();
        self.work(move |db| {
            let _active = life.active()?;
            db.execute(
                "DELETE FROM function_results WHERE partition=?1 AND query=?2",
                params![partition, key],
            )
            .map_err(|_| storage())?;
            Ok(())
        })
        .await
    }
    pub(super) async fn clear_results(&self, partition: &Partition) -> Result<(), Error> {
        let partition = partition.0.clone();
        self.work(move |db| {
            db.execute(
                "DELETE FROM function_results WHERE partition=?1",
                [partition],
            )
            .map_err(|_| storage())?;
            Ok(())
        })
        .await
    }
}
