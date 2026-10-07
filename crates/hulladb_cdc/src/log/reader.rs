//! Transaction readers and replay comparison over ordered persisted byte chunks.
use crate::{Change, Cursor, Error, Lsn};
use rusqlite::{Connection, params};
use std::io::Read;

#[derive(Debug, Clone, Copy)]
pub struct TransactionHeader {
    pub cursor: Cursor,
    pub xid: u32,
    pub commit: Lsn,
    pub end: Lsn,
    pub changes: u64,
    pub payload_bytes: u64,
}

/// Borrowed within one log read snapshot. Changes are decoded one at a time.
pub struct StoredTransaction<'a> {
    pub header: TransactionHeader,
    pub(super) db: &'a Connection,
    pub(super) id: String,
    pub(super) chunks: i64,
}
impl StoredTransaction<'_> {
    /// Consumers that publish derived rows atomically must keep their write
    /// transaction open until this returns Ok: decoding can fail after a prefix.
    pub fn for_each_change(
        &self,
        mut visit: impl FnMut(Change) -> Result<(), Error>,
    ) -> Result<(), Error> {
        let reader = ChunkReader::new(self.db, &self.id, self.chunks);
        let mut count = 0_u64;
        for change in serde_json::Deserializer::from_reader(reader).into_iter::<Change>() {
            count = count.checked_add(1).ok_or(Error::Corrupt)?;
            if count > self.header.changes {
                return Err(Error::Corrupt);
            }
            visit(change?)?;
        }
        if count != self.header.changes {
            return Err(Error::Corrupt);
        }
        Ok(())
    }
}

struct ChunkReader<'a> {
    db: &'a Connection,
    id: &'a str,
    chunks: i64,
    ordinal: i64,
    buffer: Vec<u8>,
    offset: usize,
}
impl<'a> ChunkReader<'a> {
    fn new(db: &'a Connection, id: &'a str, chunks: i64) -> Self {
        Self {
            db,
            id,
            chunks,
            ordinal: 0,
            buffer: Vec::new(),
            offset: 0,
        }
    }
}
impl Read for ChunkReader<'_> {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        if self.offset == self.buffer.len() {
            if self.ordinal == self.chunks {
                return Ok(0);
            }
            self.buffer = self
                .db
                .query_row(
                    "SELECT payload FROM batch_chunks WHERE batch_id=?1 AND ordinal=?2",
                    params![self.id, self.ordinal],
                    |r| r.get(0),
                )
                .map_err(std::io::Error::other)?;
            if self.buffer.is_empty() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "empty log chunk",
                ));
            }
            self.ordinal += 1;
            self.offset = 0;
        }
        let n = out.len().min(self.buffer.len() - self.offset);
        out[..n].copy_from_slice(&self.buffer[self.offset..self.offset + n]);
        self.offset += n;
        Ok(n)
    }
}

pub(super) fn same_chunks(
    db: &Connection,
    left: &str,
    left_chunks: i64,
    right: &str,
    right_chunks: i64,
) -> Result<bool, Error> {
    // Canonical encoding is stable across chunk-size changes. Compare bounded
    // blocks rather than joining payloads or reconstructing whole transactions.
    fn fill(reader: &mut impl Read, buffer: &mut [u8]) -> Result<usize, Error> {
        let mut used = 0;
        while used < buffer.len() {
            let n = reader.read(&mut buffer[used..]).map_err(Error::Io)?;
            if n == 0 {
                break;
            }
            used += n;
        }
        Ok(used)
    }
    let mut a = ChunkReader::new(db, left, left_chunks);
    let mut b = ChunkReader::new(db, right, right_chunks);
    let mut first = [0; 8192];
    let mut second = [0; 8192];
    loop {
        let n = fill(&mut a, &mut first)?;
        let m = fill(&mut b, &mut second)?;
        if n != m || first[..n] != second[..m] {
            return Ok(false);
        }
        if n == 0 {
            return Ok(true);
        }
    }
}
