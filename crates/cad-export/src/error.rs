//! Export errors.

/// Result alias for the writers.
pub type Result<T> = std::result::Result<T, ExportError>;

/// Everything that can go wrong writing a scene out.
#[derive(Debug, thiserror::Error)]
pub enum ExportError {
    #[error("io error writing {path}: {source}")]
    Io {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("io error: {0}")]
    RawIo(#[from] std::io::Error),

    #[error("the scene has no tessellated geometry — run the tessellator first")]
    NoMesh,

    #[error("{format} cannot represent {what}")]
    Unsupported {
        format: &'static str,
        what: String,
    },

    #[error("mesh is malformed: {0}")]
    BadMesh(String),
}
