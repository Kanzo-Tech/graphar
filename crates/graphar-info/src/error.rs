/// Everything that can go wrong loading, dumping or validating an information file.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A document is not YAML, or not an information document: a missing or unknown key, or a
    /// value of the wrong kind.
    #[error("{document}: {source}")]
    Yaml {
        /// The document: the path the graph document names it by, or its kind.
        document: String,
        /// The parser's error, which carries the line and column.
        #[source]
        source: serde_yaml_ng::Error,
    },
    /// A file could not be read.
    #[error("{path}: {source}")]
    Io {
        /// The path, as it was opened.
        path: String,
        /// The error the file system returned.
        #[source]
        source: std::io::Error,
    },
    /// The documents parse but break a rule a reader relies on.
    #[error("{at}: {reason}")]
    Invalid {
        /// Where: the vertex or edge type, and the property or list within it.
        at: String,
        /// The rule broken.
        reason: String,
    },
}
