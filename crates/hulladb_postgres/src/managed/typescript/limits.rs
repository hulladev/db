use super::{Error, Json};
use serde::{Deserialize, Serialize};

/// Trusted invocation budgets; byte limits count encoded data without envelopes.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields, default)]
pub struct InvocationLimits {
    pub execution_ms: u64,
    pub input_bytes: usize,
    pub output_bytes: usize,
    pub memory_bytes: usize,
    pub stack_bytes: usize,
    pub operations: usize,
    pub read_rows: usize,
    pub read_bytes: usize,
}
impl Default for InvocationLimits {
    fn default() -> Self {
        Self {
            execution_ms: 5000,
            input_bytes: 16 * 1024 * 1024,
            output_bytes: 16 * 1024 * 1024,
            memory_bytes: 64 * 1024 * 1024,
            stack_bytes: 512 * 1024,
            operations: 1000,
            read_rows: 100_000,
            read_bytes: 64 * 1024 * 1024,
        }
    }
}
impl InvocationLimits {
    pub fn validate(self) -> Result<Self, Error> {
        if self.execution_ms == 0
            || self.execution_ms > 60_000
            || self.input_bytes == 0
            || self.input_bytes > 64 * 1024 * 1024
            || self.output_bytes == 0
            || self.output_bytes > 64 * 1024 * 1024
            || self.memory_bytes == 0
            || self.memory_bytes > 1024 * 1024 * 1024
            || self.stack_bytes == 0
            || self.stack_bytes > 16 * 1024 * 1024
            || self.operations == 0
            || self.operations > 1_000_000
            || self.read_rows == 0
            || self.read_rows > 1_000_000
            || self.read_bytes == 0
            || self.read_bytes > 64 * 1024 * 1024
        {
            return Err(Error::InvalidInput(
                "invocation limits must be positive bounded integers".into(),
            ));
        }
        Ok(self)
    }
    pub fn merge(self, overrides: &Json) -> Result<Self, Error> {
        let mut value = serde_json::to_value(self).map_err(|_| Error::CorruptData)?;
        let object = overrides
            .as_object()
            .ok_or_else(|| Error::InvalidInput("invalid invocation limits".into()))?;
        for (key, setting) in object {
            value[key] = setting.clone();
        }
        serde_json::from_value::<Self>(value)
            .map_err(|_| Error::InvalidInput("invalid invocation limits".into()))?
            .validate()
    }
    pub fn frame_bytes(self) -> usize {
        self.input_bytes.max(self.output_bytes).max(self.read_bytes) + 2 * 1024 * 1024
    }
}
