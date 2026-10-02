use std::fmt;

/// Wire diagnostics are distinct from schema/value validation diagnostics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WireError {
    /// Invalid JSON or a top-level value that is not a record object.
    Json {
        message: String,
        line: usize,
        column: usize,
    },
    Field {
        field: String,
        kind: WireErrorKind,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireErrorKind {
    DuplicateField,
    /// JSON numbers and arrays are outside the current scalar format.
    UnsupportedValue,
    /// A scalar object must have exactly one known tag with a string payload.
    InvalidScalarTag,
    /// Noncanonical decimal text or a value outside the signed 64-bit range.
    InvalidInt64,
    /// UUID text must use the 36-character hyphenated hexadecimal form.
    InvalidUuid,
}

impl From<serde_json::Error> for WireError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json {
            message: error.to_string(),
            line: error.line(),
            column: error.column(),
        }
    }
}

impl fmt::Display for WireError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Json { message, .. } => write!(f, "invalid wire JSON: {message}"),
            Self::Field { field, kind } => {
                write!(f, "wire field {field:?}: ")?;
                match kind {
                    WireErrorKind::DuplicateField => write!(f, "field is supplied more than once"),
                    WireErrorKind::UnsupportedValue => {
                        write!(f, "use null, string, boolean, or a tagged int64/UUID")
                    }
                    WireErrorKind::InvalidScalarTag => {
                        write!(
                            f,
                            "use exactly one $int64 or $uuid tag with a string payload"
                        )
                    }
                    WireErrorKind::InvalidInt64 => {
                        write!(f, "expected canonical signed 64-bit decimal text")
                    }
                    WireErrorKind::InvalidUuid => {
                        write!(f, "expected a hyphenated 36-character hexadecimal UUID")
                    }
                }
            }
        }
    }
}

impl std::error::Error for WireError {}
