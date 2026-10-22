use std::{future::IntoFuture, marker::PhantomData};

use hulladb_postgres::managed;
use hulladb_schema::{SchemaContract, SchemaDeclaration, Value};
use serde_json::{Map, Value as Json};

use crate::{Error, Field, FunctionFuture, Id, InsertInput, PatchInput, Scalar, Stored, Table};

#[derive(Clone, Copy)]
pub struct ReadDb<'a> {
    inner: managed::ReadDb<'a>,
}
pub struct WriteDb<'a, I> {
    inner: managed::WriteDb<'a, I>,
}
pub struct QueryContext<'a, I = ()> {
    pub db: ReadDb<'a>,
    pub identity: &'a I,
}
pub struct MutationContext<'a, I = ()> {
    pub db: WriteDb<'a, I>,
    pub identity: &'a I,
}

impl<'a, I> From<managed::QueryContext<'a, I>> for QueryContext<'a, I> {
    fn from(context: managed::QueryContext<'a, I>) -> Self {
        Self {
            db: ReadDb { inner: context.db },
            identity: context.identity,
        }
    }
}
impl<'a, I> From<managed::MutationContext<'a, I>> for MutationContext<'a, I> {
    fn from(context: managed::MutationContext<'a, I>) -> Self {
        Self {
            db: WriteDb { inner: context.db },
            identity: context.identity,
        }
    }
}

impl<'a> ReadDb<'a> {
    pub fn select<T: Table>(&self) -> Select<'a, T> {
        Select {
            db: self.inner,
            select: managed::Select::default(),
            marker: PhantomData,
        }
    }
    pub async fn get<T: Table>(&self, id: Id<T>) -> Result<Option<Stored<T>>, Error> {
        ensure_table::<T>(self.inner.schema())?;
        self.inner
            .get(T::NAME, id.value())
            .await?
            .map(decode_row::<T>)
            .transpose()
    }
}

/// Schema-typed predicates. Awaiting a selector returns logical rows; use
/// `versioned()` when a caller needs versions for a subsequent edit/delete.
pub struct Select<'a, T: Table> {
    db: managed::ReadDb<'a>,
    select: managed::Select,
    marker: PhantomData<fn() -> T>,
}
impl<'a, T: Table> Select<'a, T> {
    pub fn where_eq<V: Scalar>(mut self, field: Field<T, V>, value: V) -> Self {
        self.select
            .equals
            .insert(field.name().into(), value.value());
        self
    }
    pub fn after(mut self, id: Id<T>) -> Self {
        self.select.after = Some(id.value());
        self
    }
    pub fn take(mut self, limit: u16) -> Self {
        self.select.limit = limit;
        self
    }
    pub async fn versioned(self) -> Result<Vec<Stored<T>>, Error> {
        ensure_table::<T>(self.db.schema())?;
        self.db
            .select(T::NAME, self.select)
            .await?
            .into_iter()
            .map(decode_row::<T>)
            .collect()
    }
}
impl<'a, T: Table> IntoFuture for Select<'a, T> {
    type Output = Result<Vec<T>, Error>;
    type IntoFuture = FunctionFuture<'a, Vec<T>>;
    fn into_future(self) -> Self::IntoFuture {
        Box::pin(async move {
            Ok(self
                .versioned()
                .await?
                .into_iter()
                .map(|row| row.row)
                .collect())
        })
    }
}

impl<'a, I: Send + Sync + 'static> WriteDb<'a, I> {
    /// Enqueue an external side effect atomically with this mutation. Use the
    /// returned job UUID as the delivery handler's idempotency key.
    pub async fn enqueue<P: crate::Codec>(
        &self,
        name: &str,
        payload: P,
    ) -> Result<uuid::Uuid, Error> {
        self.inner.enqueue(name, payload.encode()?).await
    }
    pub async fn enqueue_with<P: crate::Codec>(
        &self,
        name: &str,
        payload: P,
        options: managed::EnqueueOptions,
    ) -> Result<uuid::Uuid, Error> {
        self.inner
            .enqueue_with(name, payload.encode()?, options)
            .await
    }
    pub fn read(&self) -> ReadDb<'a> {
        ReadDb {
            inner: self.inner.read(),
        }
    }
    pub fn select<T: Table>(&self) -> Select<'a, T> {
        self.read().select()
    }
    pub async fn get<T: Table>(&self, id: Id<T>) -> Result<Option<Stored<T>>, Error> {
        self.read().get(id).await
    }
    pub async fn insert<U: InsertInput>(&self, input: U) -> Result<U::Table, Error> {
        Ok(self.insert_versioned(input).await?.row)
    }
    pub async fn insert_versioned<U: InsertInput>(
        &self,
        input: U,
    ) -> Result<Stored<U::Table>, Error> {
        ensure_table::<U::Table>(self.inner.read().schema())?;
        decode_row(self.inner.insert(U::Table::NAME, input.values()).await?)
    }
    pub async fn update<T: Table>(
        &self,
        id: Id<T>,
        expected_version: i64,
        patch: T::Patch,
    ) -> Result<T, Error> {
        ensure_table::<T>(self.inner.read().schema())?;
        Ok(decode_row::<T>(
            self.inner
                .update(T::NAME, id.value(), expected_version, patch.values())
                .await?,
        )?
        .row)
    }
    pub async fn delete<T: Table>(&self, id: Id<T>, expected_version: i64) -> Result<T, Error> {
        ensure_table::<T>(self.inner.read().schema())?;
        Ok(decode_row::<T>(
            self.inner
                .delete(T::NAME, id.value(), expected_version)
                .await?,
        )?
        .row)
    }
}

fn decode_row<T: Table>(row: managed::Row) -> Result<Stored<T>, Error> {
    let mut object = Map::new();
    for (name, value) in row.values {
        let value = match value {
            Value::Null => Json::Null,
            Value::Date(value)
            | Value::Timestamp(value)
            | Value::LocalTimestamp(value)
            | Value::String(value) => Json::String(value),
            Value::Boolean(value) => Json::Bool(value),
            Value::Int64(value) => Json::String(value.to_string()),
            Value::Float64(value) => Json::Number(value),
            Value::Json(value) => value,
            Value::Uuid(value) => Json::String(uuid::Uuid::from_bytes(value).to_string()),
        };
        object.insert(name, value);
    }
    Ok(Stored {
        row: T::decode(&Json::Object(object))?,
        version: row.version,
    })
}

fn ensure_table<T: Table>(schema: &SchemaContract) -> Result<(), Error> {
    let mut declaration = SchemaDeclaration::new(vec![T::declaration()]);
    declaration.default_uuid_generation = schema.default_uuid_generation();
    let normalized = declaration.normalize().map_err(|_| Error::SchemaMismatch)?;
    if normalized.table(T::NAME) != schema.table(T::NAME) {
        return Err(Error::SchemaMismatch);
    }
    let expected = T::references();
    let actual: Vec<_> = schema
        .references()
        .iter()
        .filter(|reference| reference.table() == T::NAME)
        .collect();
    if expected.len() != actual.len()
        || expected.iter().any(|reference| {
            !actual
                .iter()
                .any(|r| r.field() == reference.field && r.target_table() == reference.target_table)
        })
    {
        return Err(Error::SchemaMismatch);
    }
    Ok(())
}
