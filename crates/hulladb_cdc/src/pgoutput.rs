//! Strict subset of pgoutput protocol v1, including transaction and tuple boundaries.
use std::collections::BTreeMap;

use crate::{
    Batch, Change, Column, Error, Lsn, MAX_TRANSACTION_BYTES, MAX_TRANSACTION_CHANGES, Relation,
    Value,
};

const MAX_MESSAGE_BYTES: usize = 1024 * 1024;
const MAX_COLUMNS: usize = 1024;
const MAX_RELATIONS: usize = 1024;

#[derive(Default)]
pub(crate) struct Decoder {
    relations: BTreeMap<u32, Relation>,
    relation_sizes: BTreeMap<u32, usize>,
    pending: Option<(u32, Lsn, Vec<Change>)>,
    bytes: usize,
}

impl Decoder {
    pub(crate) fn push(&mut self, data: &[u8]) -> Result<Option<Batch>, Error> {
        if data.len() > MAX_MESSAGE_BYTES {
            return Err(Error::Limit);
        }
        self.bytes = self.bytes.checked_add(data.len()).ok_or(Error::Limit)?;
        if self.bytes > MAX_TRANSACTION_BYTES {
            return Err(Error::Limit);
        }
        let mut r = Reader(data);
        let tag = r.byte()?;
        let mut batch = None;
        match tag {
            b'B' => {
                if self.pending.is_some() {
                    return Err(Error::Protocol("nested BEGIN"));
                }
                let final_lsn = Lsn(r.u64()?);
                r.u64()?; // PostgreSQL commit timestamp; not a conflict-resolution clock.
                let xid = r.u32()?;
                self.pending = Some((xid, final_lsn, Vec::new()));
            }
            b'C' => {
                if r.byte()? != 0 {
                    return Err(Error::Protocol("unknown COMMIT flags"));
                }
                let commit = Lsn(r.u64()?);
                let end = Lsn(r.u64()?);
                r.u64()?;
                let (xid, final_lsn, changes) = self
                    .pending
                    .take()
                    .ok_or(Error::Protocol("COMMIT without BEGIN"))?;
                if commit != final_lsn || end <= commit {
                    return Err(Error::Protocol("inconsistent commit LSN"));
                }
                batch = Some(Batch {
                    xid,
                    commit,
                    end,
                    changes,
                });
                self.bytes = 0;
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
                self.relation_sizes
                    .insert(oid, serde_json::to_vec(&relation)?.len());
                self.relations.insert(oid, relation);
            }
            b'I' => {
                let relation = self.relation(r.u32()?)?;
                r.expect(b'N')?;
                let row = r.tuple(relation.columns.len())?;
                self.change(Change::Insert { relation, row })?;
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
                self.change(Change::Update { relation, old, row })?;
            }
            b'D' => {
                let relation = self.relation(r.u32()?)?;
                if !matches!(r.byte()?, b'K' | b'O') {
                    return Err(Error::Protocol("DELETE missing identity"));
                }
                let old = r.tuple(relation.columns.len())?;
                self.change(Change::Delete { relation, old })?;
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
                self.change(Change::Truncate {
                    relations,
                    cascade: flags & 1 != 0,
                    restart_identity: flags & 2 != 0,
                })?;
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
        Ok(batch)
    }

    fn relation(&mut self, oid: u32) -> Result<Relation, Error> {
        // The internal JSON envelope repeats relation metadata per change.
        // Charge that expansion before cloning, rather than bounding WAL bytes alone.
        let size = *self
            .relation_sizes
            .get(&oid)
            .ok_or(Error::Protocol("unknown relation"))?;
        self.bytes = self.bytes.checked_add(size).ok_or(Error::Limit)?;
        if self.bytes > MAX_TRANSACTION_BYTES {
            return Err(Error::Limit);
        }
        self.relations
            .get(&oid)
            .cloned()
            .ok_or(Error::Protocol("unknown relation"))
    }

    fn change(&mut self, change: Change) -> Result<(), Error> {
        let (_, _, changes) = self
            .pending
            .as_mut()
            .ok_or(Error::Protocol("row change outside transaction"))?;
        if changes.len() >= MAX_TRANSACTION_CHANGES {
            return Err(Error::Limit);
        }
        changes.push(change);
        Ok(())
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
        if self.pending.is_some() {
            self.bytes = self.bytes.checked_add(size).ok_or(Error::Limit)?;
            if self.bytes > MAX_TRANSACTION_BYTES {
                return Err(Error::Limit);
            }
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn finish(self) -> Result<(), Error> {
        if self.pending.is_some() {
            Err(Error::Protocol("incomplete transaction"))
        } else {
            Ok(())
        }
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
mod tests {
    use super::*;

    fn begin() -> Vec<u8> {
        let mut data = vec![b'B'];
        data.extend(100_u64.to_be_bytes());
        data.extend(0_u64.to_be_bytes());
        data.extend(1_u32.to_be_bytes());
        data
    }
    fn relation() -> Vec<u8> {
        let mut data = vec![b'R'];
        data.extend(7_u32.to_be_bytes());
        data.extend(b"public\0tasks\0");
        data.push(b'd');
        data.extend(1_u16.to_be_bytes());
        data.push(1);
        data.extend(b"id\0");
        data.extend(25_u32.to_be_bytes());
        data.extend(u32::MAX.to_be_bytes());
        data
    }
    fn insert() -> Vec<u8> {
        let mut data = vec![b'I'];
        data.extend(7_u32.to_be_bytes());
        data.push(b'N');
        data.extend(1_u16.to_be_bytes());
        data.push(b't');
        data.extend(3_u32.to_be_bytes());
        data.extend(b"abc");
        data
    }
    fn commit() -> Vec<u8> {
        let mut data = vec![b'C', 0];
        data.extend(100_u64.to_be_bytes());
        data.extend(101_u64.to_be_bytes());
        data.extend(0_u64.to_be_bytes());
        data
    }
    #[test]
    fn complete_transaction_and_truncated_messages() {
        let mut decoder = Decoder::default();
        for msg in [begin(), relation(), insert()] {
            assert!(decoder.push(&msg).unwrap().is_none());
        }
        let batch = decoder.push(&commit()).unwrap().unwrap();
        assert_eq!(batch.end, Lsn(101));
        assert_eq!(batch.changes.len(), 1);
        decoder.finish().unwrap();
        for message in [begin(), relation(), insert(), commit()] {
            for length in 0..message.len() {
                let mut decoder = Decoder::default();
                decoder.push(&begin()).unwrap();
                decoder.push(&relation()).unwrap();
                assert!(decoder.push(&message[..length]).is_err());
            }
        }
    }
    #[test]
    fn refuse_gaps_unknown_messages_and_incomplete_transactions() {
        assert!(Decoder::default().push(&insert()).is_err());
        assert!(Decoder::default().push(&commit()).is_err());
        assert!(Decoder::default().push(b"?").is_err());
        let mut decoder = Decoder::default();
        decoder.push(&begin()).unwrap();
        assert!(decoder.finish().is_err());
        let mut decoder = Decoder::default();
        decoder.push(&begin()).unwrap();
        decoder.push(&relation()).unwrap();
        let mut malformed = insert();
        malformed.push(0);
        assert!(decoder.push(&malformed).is_err());
    }

    #[test]
    fn tuple_null_toast_binary_and_count_limits() {
        let mut reader = Reader(&[0, 2, b'n', b'u']);
        assert_eq!(
            reader.tuple(2).unwrap(),
            vec![Value::Null, Value::UnchangedToast]
        );
        assert!(Reader(&[0, 2, b'n', b'u']).tuple(1).is_err());
        assert!(Reader(&[0, 1, b'b', 0, 0, 0, 1, 0]).tuple(1).is_err());
        assert!(Reader(&[0, 1, b't', 255, 255, 255, 255]).tuple(1).is_err());
        assert!(matches!(
            Decoder::default().push(&vec![0; MAX_MESSAGE_BYTES + 1]),
            Err(Error::Limit)
        ));
    }

    #[test]
    fn ignored_logical_messages_still_enforce_transaction_bounds() {
        let mut decoder = Decoder::default();
        assert!(decoder.ignore_message(true, 1).is_err());
        assert!(matches!(
            decoder.ignore_message(false, MAX_MESSAGE_BYTES + 1),
            Err(Error::Limit)
        ));
        decoder.ignore_message(false, MAX_MESSAGE_BYTES).unwrap();
        decoder.push(&begin()).unwrap();
        assert!(decoder.in_transaction());
        for _ in 0..3 {
            decoder.ignore_message(true, MAX_MESSAGE_BYTES).unwrap();
        }
        assert!(matches!(
            decoder.ignore_message(true, MAX_MESSAGE_BYTES),
            Err(Error::Limit)
        ));
    }
}
