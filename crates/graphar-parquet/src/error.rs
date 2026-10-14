/// Everything that can go wrong writing or reading GraphAr's data files.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The information files are invalid, or do not dump.
    #[error(transparent)]
    Info(#[from] graphar_info::Error),
    /// The store refused a read or a write.
    #[error(transparent)]
    Store(#[from] object_store::Error),
    /// A path composed from the information files is not a valid object path: a prefix with an
    /// empty segment, `.` or `..` (write `vertex/Person/`, never `./vertex/Person/`).
    #[error(transparent)]
    Path(#[from] object_store::path::Error),
    /// Parquet could not encode or decode a chunk.
    #[error(transparent)]
    Parquet(#[from] parquet::errors::ParquetError),
    /// Arrow could not assemble a batch.
    #[error(transparent)]
    Arrow(#[from] arrow_schema::ArrowError),
    /// The data handed to a writer does not match the information file, or a file read does not
    /// hold what GraphAr puts there.
    #[error("{at}: {reason}")]
    Invalid {
        /// The vertex or edge type, and the column or file within it.
        at: String,
        /// What does not match.
        reason: String,
    },
    /// A file type this crate does not read or write: it handles `parquet` only.
    #[error("{at}: file type `{file_type}` is not Parquet")]
    FileType {
        /// The vertex or edge type, and the group or list within it.
        at: String,
        /// The file type the information file names.
        file_type: graphar_info::FileType,
    },
}

pub(crate) fn invalid(at: impl Into<String>, reason: impl Into<String>) -> Error {
    Error::Invalid {
        at: at.into(),
        reason: reason.into(),
    }
}
