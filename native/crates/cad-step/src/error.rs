//! Error types for the STEP Part 21 reader.

use std::ops::Range;

/// Result alias for STEP reading.
pub type Result<T> = std::result::Result<T, StepError>;

/// Everything that can go wrong reading a Part 21 exchange file.
#[derive(Debug, thiserror::Error)]
pub enum StepError {
    #[error("io error reading {path}: {source}")]
    Io {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("not a Part 21 file: expected leading `ISO-10303-21;`")]
    NotPart21,

    #[error("unterminated {what} starting at byte {offset}")]
    Unterminated { what: &'static str, offset: usize },

    #[error("malformed record at byte {offset}: {detail}")]
    Record { offset: usize, detail: String },

    #[error("malformed value at byte {offset}: {detail}")]
    Value { offset: usize, detail: String },

    #[error("entity #{id} referenced but never defined")]
    DanglingRef { id: u32 },

    #[error("entity #{id} is {actual}, expected {expected}")]
    WrongKind {
        id: u32,
        actual: String,
        expected: &'static str,
    },

    #[error("entity #{id} ({keyword}) has {actual} arguments, expected {expected}")]
    Arity {
        id: u32,
        keyword: String,
        actual: usize,
        expected: usize,
    },
}

impl StepError {
    pub(crate) fn record(span: &Range<usize>, detail: impl Into<String>) -> Self {
        StepError::Record {
            offset: span.start,
            detail: detail.into(),
        }
    }

    pub(crate) fn value(offset: usize, detail: impl Into<String>) -> Self {
        StepError::Value {
            offset,
            detail: detail.into(),
        }
    }
}
