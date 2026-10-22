use std::{
    fmt,
    hash::{Hash, Hasher},
    marker::PhantomData,
};

use hulladb_schema::{RecordValues, ReferenceDeclaration, ScalarType, TableDeclaration, Value};
use serde_json::Value as Json;

use crate::{Codec, Error, Scalar};

pub trait Table: Codec {
    const NAME: &'static str;
    type Key: Scalar + Eq + Hash + Ord;
    type Insert: InsertInput<Table = Self>;
    type Patch: PatchInput<Table = Self>;
    fn declaration() -> TableDeclaration;
    fn references() -> Vec<ReferenceDeclaration>;
    fn id(&self) -> Id<Self>;
}

/// IDs keep table identity even when two tables share a scalar key type.
pub struct Id<T: Table> {
    key: T::Key,
    marker: PhantomData<fn() -> T>,
}
impl<T: Table> Id<T> {
    pub fn new(key: T::Key) -> Self {
        Self {
            key,
            marker: PhantomData,
        }
    }
    pub fn key(&self) -> &T::Key {
        &self.key
    }
}
impl<T: Table> Clone for Id<T> {
    fn clone(&self) -> Self {
        Self::new(self.key.clone())
    }
}
impl<T: Table> Copy for Id<T> where T::Key: Copy {}
impl<T: Table> fmt::Debug for Id<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple(T::NAME).field(&self.key.value()).finish()
    }
}
impl<T: Table> PartialEq for Id<T> {
    fn eq(&self, other: &Self) -> bool {
        self.key == other.key
    }
}
impl<T: Table> Eq for Id<T> {}
impl<T: Table> PartialOrd for Id<T> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl<T: Table> Ord for Id<T> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.key.cmp(&other.key)
    }
}
impl<T: Table> Hash for Id<T> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.key.hash(state);
    }
}
impl<T: Table> Codec for Id<T> {
    fn encode(&self) -> Result<Json, Error> {
        self.key.encode()
    }
    fn decode(value: &Json) -> Result<Self, Error> {
        T::Key::decode(value).map(Self::new)
    }
}
impl<T: Table> Scalar for Id<T> {
    const TYPE: ScalarType = T::Key::TYPE;
    fn value(&self) -> Value {
        self.key.value()
    }
}

pub trait InsertInput: Codec {
    type Table: Table;
    fn values(&self) -> RecordValues;
}
pub trait PatchInput: Codec {
    type Table: Table;
    fn values(&self) -> RecordValues;
}

/// Storage versions stay separate from the logical authored row.
#[derive(Debug)]
pub struct Stored<T> {
    pub row: T,
    pub version: i64,
}

pub struct Field<T: Table, V: Scalar> {
    #[cfg_attr(not(feature = "server"), allow(dead_code))]
    name: &'static str,
    marker: PhantomData<fn() -> (T, V)>,
}
impl<T: Table, V: Scalar> Field<T, V> {
    #[doc(hidden)]
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            marker: PhantomData,
        }
    }
    #[cfg(feature = "server")]
    pub(crate) fn name(&self) -> &'static str {
        self.name
    }
}
impl<T: Table, V: Scalar> Copy for Field<T, V> {}
impl<T: Table, V: Scalar> Clone for Field<T, V> {
    fn clone(&self) -> Self {
        *self
    }
}
