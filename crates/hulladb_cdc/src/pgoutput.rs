//! Strict subset of pgoutput protocol v1, including transaction and tuple boundaries.
use std::collections::BTreeMap;

use crate::{Change, Column, Error, Lsn, Relation, Value};

const MAX_MESSAGE_BYTES: usize = 1024 * 1024;
const MAX_COLUMNS: usize = 1024;
const MAX_RELATIONS: usize = 1024;

#[derive(Default)]
pub(crate) struct Decoder {
    relations: BTreeMap<u32, Relation>,
    pending: Option<Lsn>,
}

pub(crate) enum DecodedEvent {
    Begin { xid: u32, commit: Lsn },
    Change(Change),
    Commit { commit: Lsn, end: Lsn },
}

impl Decoder {
    pub(crate) fn push(&mut self, data: &[u8]) -> Result<Option<DecodedEvent>, Error> {
        if data.len() > MAX_MESSAGE_BYTES {
            return Err(Error::Limit);
        }
        let mut r = Reader(data);
        let tag = r.byte()?;
        let mut event = None;
        match tag {
            b'B' => {
                if self.pending.is_some() {
                    return Err(Error::Protocol("nested BEGIN"));
                }
                let final_lsn = Lsn(r.u64()?);
                r.u64()?; // PostgreSQL commit timestamp; not a conflict-resolution clock.
                let xid = r.u32()?;
                self.pending = Some(final_lsn);
                event = Some(DecodedEvent::Begin {
                    xid,
                    commit: final_lsn,
                });
            }
            b'C' => {
                if r.byte()? != 0 {
                    return Err(Error::Protocol("unknown COMMIT flags"));
                }
                let commit = Lsn(r.u64()?);
                let end = Lsn(r.u64()?);
                r.u64()?;
                let final_lsn = self
                    .pending
                    .take()
                    .ok_or(Error::Protocol("COMMIT without BEGIN"))?;
                if commit != final_lsn || end <= commit {
                    return Err(Error::Protocol("inconsistent commit LSN"));
                }
                event = Some(DecodedEvent::Commit { commit, end });
            }
            b'R' => {
                let oid = r.u32()?;
                let namespace = r.string()?;
                let name = r.string()?;
                let replica_identity = r.byte()?;
                if !matches!(replica_identity, b'd' | b'n' | b'f' | b'i') {
                    return Err(Error::Protocol("unknown replica identity"));
                }
                let count = usize::from(r.u16()?);
                if count > MAX_COLUMNS {
                    return Err(Error::Limit);
                }
                let mut columns = Vec::with_capacity(count);
                for _ in 0..count {
                    let flags = r.byte()?;
                    if flags > 1 {
                        return Err(Error::Protocol("unknown column flags"));
                    }
                    columns.push(Column {
                        key: flags == 1,
                        name: r.string()?,
                        type_oid: r.u32()?,
                        type_modifier: r.u32()? as i32,
                    });
                }
                if !self.relations.contains_key(&oid) && self.relations.len() >= MAX_RELATIONS {
                    return Err(Error::Limit);
                }
                let relation = Relation {
                    oid,
                    namespace,
                    name,
                    replica_identity,
                    columns,
                };
                self.relations.insert(oid, relation);
            }
            b'I' => {
                let relation = self.relation(r.u32()?)?;
                r.expect(b'N')?;
                let row = r.tuple(relation.columns.len())?;
                event = Some(self.change(Change::Insert { relation, row })?);
            }
            b'U' => {
                let relation = self.relation(r.u32()?)?;
                let mut kind = r.byte()?;
                let old = if matches!(kind, b'K' | b'O') {
                    let row = r.tuple(relation.columns.len())?;
                    kind = r.byte()?;
                    Some(row)
                } else {
                    None
                };
                if kind != b'N' {
                    return Err(Error::Protocol("UPDATE missing new tuple"));
                }
                let row = r.tuple(relation.columns.len())?;
                event = Some(self.change(Change::Update { relation, old, row })?);
            }
            b'D' => {
                let relation = self.relation(r.u32()?)?;
                if !matches!(r.byte()?, b'K' | b'O') {
                    return Err(Error::Protocol("DELETE missing identity"));
                }
                let old = r.tuple(relation.columns.len())?;
                event = Some(self.change(Change::Delete { relation, old })?);
            }
            b'T' => {
                let count = usize::try_from(r.u32()?).map_err(|_| Error::Limit)?;
                if count > MAX_RELATIONS {
                    return Err(Error::Limit);
                }
                let flags = r.byte()?;
                if flags & !3 != 0 {
                    return Err(Error::Protocol("unknown TRUNCATE flags"));
                }
                let mut relations = Vec::with_capacity(count);
                for _ in 0..count {
                    relations.push(self.relation(r.u32()?)?);
                }
                event = Some(self.change(Change::Truncate {
                    relations,
                    cascade: flags & 1 != 0,
                    restart_identity: flags & 2 != 0,
                })?);
            }
            b'O' => {
                r.u64()?;
                r.string()?;
            }
            b'Y' => {
                r.u32()?;
                r.string()?;
                r.string()?;
            }
            _ => return Err(Error::Protocol("unsupported pgoutput message")),
        }
        if !r.0.is_empty() {
            return Err(Error::Protocol("trailing pgoutput bytes"));
        }
        Ok(event)
    }

    fn relation(&mut self, oid: u32) -> Result<Relation, Error> {
        self.relations
            .get(&oid)
            .cloned()
            .ok_or(Error::Protocol("unknown relation"))
    }

    fn change(&self, change: Change) -> Result<DecodedEvent, Error> {
        if self.pending.is_none() {
            return Err(Error::Protocol("row change outside transaction"));
        }
        Ok(DecodedEvent::Change(change))
    }

    pub(crate) fn in_transaction(&self) -> bool {
        self.pending.is_some()
    }

    pub(crate) fn ignore_message(&mut self, transactional: bool, size: usize) -> Result<(), Error> {
        if size > MAX_MESSAGE_BYTES {
            return Err(Error::Limit);
        }
        if transactional && self.pending.is_none() {
            return Err(Error::Protocol("transactional message outside transaction"));
        }
        Ok(())
    }
}

struct Reader<'a>(&'a [u8]);
impl Reader<'_> {
    fn take(&mut self, count: usize) -> Result<&[u8], Error> {
        if count > self.0.len() {
            return Err(Error::Protocol("truncated pgoutput message"));
        }
        let (head, tail) = self.0.split_at(count);
        self.0 = tail;
        Ok(head)
    }
    fn byte(&mut self) -> Result<u8, Error> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, Error> {
        Ok(u16::from_be_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn u32(&mut self) -> Result<u32, Error> {
        Ok(u32::from_be_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn u64(&mut self) -> Result<u64, Error> {
        Ok(u64::from_be_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn expect(&mut self, byte: u8) -> Result<(), Error> {
        if self.byte()? != byte {
            return Err(Error::Protocol("unexpected tuple tag"));
        }
        Ok(())
    }
    fn string(&mut self) -> Result<String, Error> {
        let length = self
            .0
            .iter()
            .position(|b| *b == 0)
            .ok_or(Error::Protocol("unterminated pgoutput string"))?;
        let s = std::str::from_utf8(self.take(length)?)
            .map_err(|_| Error::Protocol("non-UTF8 pgoutput string"))?
            .to_owned();
        self.take(1)?;
        Ok(s)
    }
    fn tuple(&mut self, count: usize) -> Result<Vec<Value>, Error> {
        if usize::from(self.u16()?) != count {
            return Err(Error::Protocol("tuple column count differs"));
        }
        let mut row = Vec::with_capacity(count);
        for _ in 0..count {
            row.push(match self.byte()? {
                b'n' => Value::Null,
                b'u' => Value::UnchangedToast,
                b't' => {
                    let count = usize::try_from(self.u32()?).map_err(|_| Error::Limit)?;
                    Value::Text(
                        std::str::from_utf8(self.take(count)?)
                            .map_err(|_| Error::Protocol("non-UTF8 tuple"))?
                            .to_owned(),
                    )
                }
                _ => return Err(Error::Protocol("unsupported tuple format")),
            });
        }
        Ok(row)
    }
}

#[cfg(test)]
#[path = "../tests/unit/pgoutput.rs"]
mod tests;
