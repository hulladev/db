use futures_util::TryStreamExt;
use serde::{Deserialize, Serialize};

use crate::{Capture, Column, Error, Lsn, Relation, Source, Value};

pub const MAX_SNAPSHOT_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_SNAPSHOT_ROWS: usize = 100_000;

/// A complete internal snapshot, never an authorization-filtered client result.
/// Constructed only after the repeatable-read copy has completed successfully.
pub struct Snapshot {
    pub(crate) source: Source,
    pub(crate) visibility: Visibility,
    pub(crate) through: Lsn,
    pub(crate) tables: Vec<(Relation, Vec<Vec<Value>>)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Visibility {
    xmin: u32,
    xmax: u32,
    active: Vec<u32>,
}

impl Visibility {
    pub(crate) fn parse(text: &str) -> Result<Self, Error> {
        let mut parts = text.split(':');
        let xmin = parts
            .next()
            .ok_or(Error::Corrupt)?
            .parse::<u32>()
            .map_err(|_| {
                Error::Invalid("snapshot XID epoch is unsupported; exported bootstrap required")
            })?;
        let xmax = parts
            .next()
            .ok_or(Error::Corrupt)?
            .parse::<u32>()
            .map_err(|_| {
                Error::Invalid("snapshot XID epoch is unsupported; exported bootstrap required")
            })?;
        let list = parts.next().ok_or(Error::Corrupt)?;
        if parts.next().is_some() || xmin < 3 || xmax < xmin {
            return Err(Error::Corrupt);
        }
        let active: Vec<u32> = if list.is_empty() {
            Vec::new()
        } else {
            list.split(',')
                .map(|v| v.parse().map_err(|_| Error::Corrupt))
                .collect::<Result<_, _>>()?
        };
        if active.iter().any(|x| *x < xmin || *x >= xmax)
            || active.windows(2).any(|pair| pair[0] >= pair[1])
        {
            return Err(Error::Corrupt);
        }
        Ok(Self { xmin, xmax, active })
    }

    pub(crate) fn visible(&self, xid: u32) -> Result<bool, Error> {
        if xid < 3 {
            return Err(Error::Corrupt);
        }
        Ok(xid < self.xmin || (xid < self.xmax && self.active.binary_search(&xid).is_err()))
    }
}

/// Owns the slot cooperatively while copying a stable MVCC snapshot.
/// The caller creates a fresh dedicated slot first; existing slots are never reset.
pub struct SnapshotReader {
    capture: Capture,
    visibility: Visibility,
    through: Lsn,
    locked: Vec<u32>,
}

impl SnapshotReader {
    pub async fn begin(url: &str, source: &Source) -> Result<Self, Error> {
        let capture = Capture::connect(url, source).await?;
        capture.check_slot(source.start).await?;
        let confirmed: String = capture
            .db
            .query_one(
                "SELECT confirmed_flush_lsn::text FROM pg_replication_slots WHERE slot_name=$1",
                &[&source.slot],
            )
            .await?
            .get(0);
        if confirmed.parse::<Lsn>()? != source.start {
            return Err(Error::SlotAhead);
        }
        let tables = capture
            .db
            .query(
                "SELECT c.oid,n.nspname,c.relname FROM pg_publication_rel pr
             JOIN pg_class c ON c.oid=pr.prrelid JOIN pg_namespace n ON n.oid=c.relnamespace
             WHERE pr.prpubid=$1 ORDER BY c.oid",
                &[&source.publication_oid],
            )
            .await?;
        if tables.is_empty() || tables.len() > 64 {
            return Err(Error::Limit);
        }
        let locked = tables.iter().map(|r| r.get(0)).collect();
        let names = tables
            .iter()
            .map(|r| {
                format!(
                    "{}.{}",
                    quote(&r.get::<_, String>(1)),
                    quote(&r.get::<_, String>(2))
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        capture.db.batch_execute(&format!("BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY; LOCK TABLE {names} IN ACCESS SHARE MODE")).await?;
        // The statement acquires its snapshot before evaluating the WAL position.
        // Every commit visible to this snapshot precedes this insert boundary.
        let row = capture
            .db
            .query_one(
                "SELECT pg_current_snapshot()::text, pg_current_wal_insert_lsn()::text",
                &[],
            )
            .await?;
        let visibility = Visibility::parse(&row.get::<_, String>(0))?;
        let through = row.get::<_, String>(1).parse()?;
        // Allocate an XID after sampling WAL. Reject crossing the first XID
        // wrap during acquisition as well as databases already in later epochs.
        let anchor: String = capture
            .db
            .query_one("SELECT pg_current_xact_id()::text", &[])
            .await?
            .get(0);
        anchor.parse::<u32>().map_err(|_| {
            Error::Invalid("snapshot XID epoch is unsupported; exported bootstrap required")
        })?;
        Ok(Self {
            capture,
            visibility,
            through,
            locked,
        })
    }

    /// Bounded copy of explicit ordinary tables with default primary-key identity.
    /// DDL/publication changes require a fresh generation, not implicit adaptation.
    pub async fn read(self) -> Result<Snapshot, Error> {
        let db = &self.capture.db;
        let publication = db.query_one(
            "SELECT puballtables,pubinsert,pubupdate,pubdelete,pubtruncate,pubviaroot,pubgencols::text,
             EXISTS(SELECT 1 FROM pg_publication_namespace WHERE pnpubid=pg_publication.oid)
             FROM pg_publication WHERE oid=$1", &[&self.capture.source.publication_oid],
        ).await?;
        if publication.get::<_, bool>(0)
            || !(1..=4).all(|i| publication.get::<_, bool>(i))
            || publication.get::<_, bool>(5)
            || publication.get::<_, String>(6) != "n"
            || publication.get::<_, bool>(7)
        {
            return Err(Error::Invalid(
                "bootstrap requires an explicit unfiltered all-operation publication",
            ));
        }
        let tables = db
            .query(
                "SELECT c.oid,n.nspname,c.relname,c.relkind::text,c.relreplident::text,
             pr.prattrs IS NOT NULL OR pr.prqual IS NOT NULL,
             EXISTS(SELECT 1 FROM pg_inherits WHERE inhrelid=c.oid OR inhparent=c.oid),c.relrowsecurity
             FROM pg_publication_rel pr JOIN pg_class c ON c.oid=pr.prrelid
             JOIN pg_namespace n ON n.oid=c.relnamespace WHERE pr.prpubid=$1 ORDER BY c.oid",
                &[&self.capture.source.publication_oid],
            )
            .await?;
        if tables.is_empty() || tables.len() > 64 {
            return Err(Error::Limit);
        }
        if tables
            .iter()
            .map(|r| r.get::<_, u32>(0))
            .collect::<Vec<_>>()
            != self.locked
        {
            return Err(Error::SourceMismatch);
        }
        let mut copied = Vec::new();
        let mut bytes = 0_usize;
        let mut count = 0_usize;
        for table in tables {
            if table.get::<_, String>(3) != "r"
                || table.get::<_, String>(4) != "d"
                || table.get::<_, bool>(5)
                || table.get::<_, bool>(6)
                || table.get::<_, bool>(7)
            {
                return Err(Error::Invalid(
                    "bootstrap supports ordinary unfiltered tables with default primary-key identity",
                ));
            }
            let oid: u32 = table.get(0);
            let namespace: String = table.get(1);
            let name: String = table.get(2);
            let qualified = format!("{}.{}", quote(&namespace), quote(&name));
            let fields = db.query(
                "SELECT a.attname,a.atttypid,a.atttypmod,
                 EXISTS(SELECT 1 FROM pg_index i WHERE i.indrelid=a.attrelid AND i.indisprimary AND a.attnum=ANY(i.indkey)),
                 a.attgenerated::text FROM pg_attribute a
                 WHERE a.attrelid=$1 AND a.attnum>0 AND NOT a.attisdropped ORDER BY a.attnum", &[&oid],
            ).await?;
            if fields.is_empty() || fields.len() > 128 {
                return Err(Error::Limit);
            }
            let mut columns = Vec::new();
            for field in fields {
                if !field.get::<_, String>(4).is_empty() {
                    return Err(Error::Invalid(
                        "generated columns require explicit bootstrap support",
                    ));
                }
                let type_oid = field.get::<_, u32>(1);
                if !matches!(type_oid, 16 | 20 | 25 | 2950) {
                    return Err(Error::Invalid(
                        "bootstrap supports boolean, bigint, text, and UUID columns",
                    ));
                }
                columns.push(Column {
                    name: field.get(0),
                    type_oid: field.get(1),
                    type_modifier: field.get(2),
                    key: field.get(3),
                });
            }
            if !columns.iter().any(|c| c.key)
                || columns
                    .iter()
                    .any(|c| c.key && !matches!(c.type_oid, 20 | 25 | 2950))
            {
                return Err(Error::Invalid("bootstrap requires a primary key"));
            }
            let relation = Relation {
                oid,
                namespace,
                name,
                replica_identity: b'd',
                columns,
            };
            bytes += serde_json::to_vec(&relation)?.len();
            let expressions = relation
                .columns
                .iter()
                .map(|c| {
                    let field = quote(&c.name);
                    // Explicit bool-to-text casts use true/false, whereas the
                    // pgoutput type output function uses t/f. format uses that
                    // same native output function; guard nulls separately.
                    format!("CASE WHEN {field} IS NULL THEN NULL ELSE format('%s',{field}) END")
                })
                .collect::<Vec<_>>()
                .join(",");
            let stream = db
                .query_raw(
                    &format!("SELECT ARRAY[{expressions}] FROM {qualified}"),
                    std::iter::empty::<&(dyn tokio_postgres::types::ToSql + Sync)>(),
                )
                .await?;
            futures_util::pin_mut!(stream);
            let mut rows = Vec::new();
            while let Some(row) = stream.try_next().await? {
                let values = row
                    .get::<_, Vec<Option<String>>>(0)
                    .into_iter()
                    .map(|v| v.map_or(Value::Null, Value::Text))
                    .collect::<Vec<_>>();
                bytes = bytes
                    .checked_add(serde_json::to_vec(&values)?.len())
                    .ok_or(Error::Limit)?;
                count += 1;
                if bytes > MAX_SNAPSHOT_BYTES || count > MAX_SNAPSHOT_ROWS {
                    return Err(Error::Limit);
                }
                rows.push(values);
            }
            copied.push((relation, rows));
        }
        db.batch_execute("COMMIT").await?;
        let source = self.capture.source.clone();
        self.capture.close().await?;
        Ok(Snapshot {
            source,
            visibility: self.visibility,
            through: self.through,
            tables: copied,
        })
    }
}

fn quote(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn visibility_uses_active_transactions_and_rejects_unknown_epochs() {
        let snapshot = Visibility::parse("10:20:10,14,15").unwrap();
        for xid in [3, 9, 11, 13, 16, 19] {
            assert!(snapshot.visible(xid).unwrap());
        }
        for xid in [10, 14, 15, 20, 21] {
            assert!(!snapshot.visible(xid).unwrap());
        }
        for text in [
            "",
            "1:2:",
            "20:10:",
            "10:20:9",
            "10:20:14,10",
            "10:20:10,10",
            "10:4294967296:",
        ] {
            assert!(Visibility::parse(text).is_err(), "{text}");
        }
        assert_eq!(quote("a\"b"), "\"a\"\"b\"");
    }
}
