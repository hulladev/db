//! Decode the shared, backend-neutral schema contract before compiling PostgreSQL storage.
use super::super::{Error, ManagedSchema};
use serde_json::Value as Json;

#[doc(hidden)]
pub fn schema_from_descriptor(namespace: &str, descriptor: Json) -> Result<ManagedSchema, Error> {
    let contract = hulladb_schema::descriptor::schema_from_descriptor(descriptor)
        .map_err(|error| Error::InvalidInput(error.to_string()))?;
    if contract.tables().iter().any(|table| {
        matches!(
            table.key_generation(),
            hulladb_schema::KeyGeneration::Custom { .. }
        )
    }) {
        return Err(Error::InvalidInput(
            "direct database access does not support custom key generators".into(),
        ));
    }
    ManagedSchema::new(namespace, contract)
}
