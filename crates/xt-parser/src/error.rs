use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum XtError {
    #[error("invalid header: {0}")]
    InvalidHeader(String),

    #[error("unexpected end of input")]
    UnexpectedEof,

    #[error("parse error at byte {offset}: {detail}")]
    Parse { offset: usize, detail: String },

    #[error("unknown entity type {type_id} at index {index}")]
    UnknownEntityType { type_id: u16, index: usize },

    #[error("missing entity at index {0}")]
    MissingEntity(usize),

    #[error("invalid geometry: {0}")]
    InvalidGeometry(String),

    #[error("topology error: {0}")]
    Topology(String),

    #[error("unsupported encoding: {0}")]
    UnsupportedEncoding(String),

    #[error("io error reading {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
}

pub type Result<T> = std::result::Result<T, XtError>;

impl From<winnow::error::ErrMode<winnow::error::ContextError>> for XtError {
    fn from(e: winnow::error::ErrMode<winnow::error::ContextError>) -> Self {
        match e {
            winnow::error::ErrMode::Incomplete(_) => XtError::UnexpectedEof,
            winnow::error::ErrMode::Backtrack(c) | winnow::error::ErrMode::Cut(c) => {
                XtError::Parse {
                    offset: 0,
                    detail: format!("{:?}", c),
                }
            }
        }
    }
}
