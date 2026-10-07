//! Incremental JSON encoding and hidden SQLite chunk writes. Each flush is
//! durable; publication belongs to EventLog's final checkpoint transaction.
use super::StorageOptions;
use crate::{Change, Error, Lsn};
use rusqlite::{Connection, params};
use std::io::Write;
use uuid::Uuid;

pub(super) struct Staging {
    pub(super) id: String,
    pub(super) xid: u32,
    pub(super) commit: Lsn,
    pub(super) changes: u64,
    pub(super) bytes: u64,
    pub(super) chunks: i64,
    pub(super) buffer: Vec<u8>,
    pub(super) failed: bool,
}

pub(super) fn new_stage(xid: u32, commit: Lsn) -> Staging {
    Staging {
        id: Uuid::now_v7().to_string(),
        xid,
        commit,
        changes: 0,
        bytes: 0,
        chunks: 0,
        buffer: Vec::new(),
        failed: false,
    }
}

pub(super) fn ensure_stage(db: &Connection, stage: &Staging) -> Result<(), Error> {
    db.execute(
        "INSERT OR IGNORE INTO batches VALUES(?1,NULL,?2,?3,NULL,0,0,0)",
        params![stage.id, stage.xid, stage.commit.to_string()],
    )?;
    Ok(())
}

pub(super) fn flush_chunk(db: &Connection, stage: &mut Staging) -> Result<(), Error> {
    if stage.buffer.is_empty() {
        return Ok(());
    }
    let persist = |db: &Connection| -> Result<(), Error> {
        ensure_stage(db, stage)?;
        db.execute(
            "INSERT INTO batch_chunks VALUES(?1,?2,?3)",
            params![stage.id, stage.chunks, stage.buffer],
        )?;
        db.execute("UPDATE batches SET payload_bytes=payload_bytes+?2,chunk_count=chunk_count+1 WHERE id=?1", params![stage.id,stage.buffer.len() as i64])?;
        Ok(())
    };
    if db.is_autocommit() {
        let tx = db.unchecked_transaction()?;
        persist(&tx)?;
        tx.commit()?;
    } else {
        // Publication already holds an outer transaction.
        persist(db)?;
    }
    stage.buffer.clear();
    stage.chunks = stage.chunks.checked_add(1).ok_or(Error::Corrupt)?;
    Ok(())
}

pub(super) fn write_change(
    db: &Connection,
    stage: &mut Staging,
    options: StorageOptions,
    change: &Change,
) -> Result<(), Error> {
    struct Encoder<'a> {
        db: &'a Connection,
        stage: &'a mut Staging,
        options: StorageOptions,
        failure: Option<Error>,
    }
    impl Write for Encoder<'_> {
        fn write(&mut self, mut data: &[u8]) -> std::io::Result<usize> {
            let len = data.len();
            let result = (|| {
                let used = self
                    .stage
                    .bytes
                    .checked_add(len as u64)
                    .ok_or(Error::Corrupt)?;
                if used > i64::MAX as u64
                    || self
                        .options
                        .max_staged_payload_bytes
                        .is_some_and(|limit| used > limit)
                {
                    return Err(Error::StagingBudget {
                        used,
                        limit: self
                            .options
                            .max_staged_payload_bytes
                            .unwrap_or(i64::MAX as u64),
                    });
                }
                self.stage.bytes = used;
                while !data.is_empty() {
                    let count = data
                        .len()
                        .min(self.options.chunk_bytes - self.stage.buffer.len());
                    self.stage.buffer.extend_from_slice(&data[..count]);
                    data = &data[count..];
                    if self.stage.buffer.len() == self.options.chunk_bytes {
                        flush_chunk(self.db, self.stage)?;
                    }
                }
                Ok(())
            })();
            if let Err(error) = result {
                self.failure = Some(error);
                return Err(std::io::Error::other("staging write failed"));
            }
            Ok(len)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut encoder = Encoder {
        db,
        stage,
        options,
        failure: None,
    };
    let result = serde_json::to_writer(&mut encoder, change)
        .and_then(|()| encoder.write_all(b"\n").map_err(serde_json::Error::io));
    if let Some(error) = encoder.failure {
        return Err(error);
    }
    result?;
    stage.changes = stage
        .changes
        .checked_add(1)
        .filter(|n| *n <= i64::MAX as u64)
        .ok_or(Error::Corrupt)?;
    Ok(())
}
