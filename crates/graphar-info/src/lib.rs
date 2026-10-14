//! Apache GraphAr's information files: the graph, vertex and edge YAML documents that describe a
//! graph stored as GraphAr, as a model you can load, dump and validate.
//!
//! The reference is the [GraphAr format specification], and where it is silent, the C++ library's
//! `graph_info.cc` (`apache/incubator-graphar` v0.13.0), which is where the YAML keys are defined.
//! The names here are GraphAr's own.
//!
//! The crate does no IO of its own. [`GraphInfo::load`] takes the graph document and a function
//! that returns the text of each vertex and edge document it names, and [`GraphInfo::dump`] returns
//! every document as `(file name, text)`. [`GraphInfo::load_file`] is the one convenience that
//! reads a local file system.
//!
//! [GraphAr format specification]: https://graphar.apache.org/docs/specification/format

mod error;
mod validate;
mod yaml;

use std::fmt;
use std::str::FromStr;

pub use error::Error;

/// A graph: the `<name>.graph.yml` document, with the vertex and edge documents it names loaded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GraphInfo {
    /// The graph's name, which also names its file: `<name>.graph.yml`.
    pub name: String,
    /// The root of the data files. `None` is GraphAr's default: the directory that holds the graph
    /// document. A prefix the document writes is used as written.
    pub prefix: Option<String>,
    /// The vertex types, in the order the document lists them.
    pub vertices: Vec<VertexInfo>,
    /// The edge types, in the order the document lists them.
    pub edges: Vec<EdgeInfo>,
    /// The graph's labels. Empty is written as no key.
    pub labels: Vec<String>,
    /// The GraphAr version the document declares.
    pub version: InfoVersion,
    /// User-defined `key`/`value` pairs, carried in order and never interpreted.
    pub extra_info: Vec<(String, String)>,
}

/// A vertex type: one `<type>.vertex.yml` document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VertexInfo {
    /// The vertex type (the specification's "label").
    pub r#type: String,
    /// The number of vertices in each chunk.
    pub chunk_size: u64,
    /// The path of this type's data files, under the graph's prefix. Absent in the document, it
    /// is `<type>/`.
    pub prefix: String,
    /// The labels a vertex of this type may carry. Empty is written as no key.
    pub labels: Vec<String>,
    /// The property groups, each stored in its own files.
    pub property_groups: Vec<PropertyGroup>,
    /// The GraphAr version the document declares.
    pub version: InfoVersion,
}

/// An edge type: one `<src_type>_<edge_type>_<dst_type>.edge.yml` document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EdgeInfo {
    /// The source vertex type.
    pub src_type: String,
    /// The edge type.
    pub edge_type: String,
    /// The destination vertex type.
    pub dst_type: String,
    /// The number of edges in each chunk.
    pub chunk_size: u64,
    /// The source vertex type's chunk size, which the offset chunks align with.
    pub src_chunk_size: u64,
    /// The destination vertex type's chunk size, which the offset chunks align with.
    pub dst_chunk_size: u64,
    /// Whether the edges are directed. Absent in the document, it is `false`.
    pub directed: bool,
    /// The path of this type's data files, under the graph's prefix. Absent in the document, it
    /// is `<src_type>_<edge_type>_<dst_type>/`.
    pub prefix: String,
    /// The adjacency lists stored, at most one of each [`AdjListType`].
    pub adj_lists: Vec<AdjacentList>,
    /// The property groups, shared by every adjacency list.
    pub property_groups: Vec<PropertyGroup>,
    /// The GraphAr version the document declares.
    pub version: InfoVersion,
}

/// One stored adjacency list of an edge type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdjacentList {
    /// How the edges are partitioned and ordered.
    pub r#type: AdjListType,
    /// The format of its data files.
    pub file_type: FileType,
    /// The path of its data files, under the edge type's prefix. Absent in the document, it is
    /// the type's name and a slash, as `ordered_by_source/`.
    pub prefix: String,
}

/// A group of properties stored together in one set of files.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PropertyGroup {
    /// The properties, in column order.
    pub properties: Vec<Property>,
    /// The format of its data files.
    pub file_type: FileType,
    /// The path of its data files, under the vertex or edge type's prefix. Absent in the document,
    /// it is the property names joined by `_`, and a slash.
    pub prefix: String,
}

/// One property: a column of a vertex or edge type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Property {
    /// The property's name.
    pub name: String,
    /// Its type.
    pub data_type: DataType,
    /// Whether it is the primary key. Absent in the document, it is `false`.
    pub is_primary: bool,
    /// Whether it may be null. Absent in the document, it is `!is_primary`; a primary key is
    /// never nullable.
    pub is_nullable: bool,
    /// How many values one vertex holds. Edge properties are always [`Cardinality::Single`].
    pub cardinality: Cardinality,
}

/// How many values of a property one vertex holds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Cardinality {
    /// One value. The default, and written as no key.
    #[default]
    Single,
    /// Several values, in order, duplicates allowed.
    List,
    /// Several distinct values.
    Set,
}

/// A property's type: the specification's built-in types, or a type the document's
/// [`InfoVersion`] declares.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum DataType {
    /// `bool`.
    Bool,
    /// `int32`.
    Int32,
    /// `int64`.
    Int64,
    /// `float`: 32-bit floating point.
    Float,
    /// `double`: 64-bit floating point.
    Double,
    /// `string`.
    String,
    /// `date`: days since the Unix epoch.
    Date,
    /// `timestamp`: milliseconds since the Unix epoch.
    Timestamp,
    /// `time`: milliseconds since midnight.
    Time,
    /// `list<T>`: a list of values of one type.
    List(Box<DataType>),
    /// Any other name. Valid only where the document's version declares it.
    UserDefined(String),
}

/// How an adjacency list partitions and orders its edges.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AdjListType {
    /// `unordered_by_source`: partitioned by source, unordered within (COO).
    UnorderedBySource,
    /// `ordered_by_source`: partitioned and ordered by source (CSR).
    OrderedBySource,
    /// `unordered_by_dest`: partitioned by destination, unordered within (COO).
    UnorderedByDest,
    /// `ordered_by_dest`: partitioned and ordered by destination (CSC).
    OrderedByDest,
}

/// The format of a group's data files.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FileType {
    /// `csv`.
    Csv,
    /// `json`.
    Json,
    /// `parquet`.
    Parquet,
    /// `orc`.
    Orc,
}

/// A document's `version`: `gar/v1`, optionally followed by the user-defined types it declares,
/// as `gar/v1 (point,polygon)`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct InfoVersion {
    /// The version number. `1` is the only one GraphAr defines.
    pub version: u32,
    /// The user-defined type names the document may use as a [`DataType`].
    pub user_define_types: Vec<String>,
}

impl Default for InfoVersion {
    fn default() -> Self {
        InfoVersion {
            version: 1,
            user_define_types: Vec::new(),
        }
    }
}

impl GraphInfo {
    /// Parses a graph document, and each vertex and edge document it names through `read`, which
    /// is handed each path exactly as the document writes it (relative to the graph document).
    /// It does not validate: call [`GraphInfo::validate`].
    pub fn load(
        yaml: &str,
        mut read: impl FnMut(&str) -> Result<String, Error>,
    ) -> Result<Self, Error> {
        yaml::load_graph(yaml, &mut read)
    }

    /// Reads a graph document from the local file system, resolving the documents it names
    /// against the directory that holds it. It does not validate.
    pub fn load_file(path: impl AsRef<std::path::Path>) -> Result<Self, Error> {
        let path = path.as_ref();
        let read_file = |p: &std::path::Path| {
            std::fs::read_to_string(p).map_err(|source| Error::Io {
                path: p.display().to_string(),
                source,
            })
        };
        let dir = path.parent().unwrap_or(std::path::Path::new(""));
        GraphInfo::load(&read_file(path)?, |name| read_file(&dir.join(name)))
    }

    /// Validates, then returns every document of the graph as `(file name, text)`: each vertex
    /// document, each edge document, and the graph document last. The graph document names the
    /// others by these file names.
    pub fn dump(&self) -> Result<Vec<(String, String)>, Error> {
        self.validate()?;
        yaml::dump_graph(self)
    }

    /// The graph document's file name, `<name>.graph.yml`.
    pub fn file_name(&self) -> String {
        format!("{}.graph.yml", self.name)
    }

    /// Checks the rules a reader relies on, and returns the first one broken.
    pub fn validate(&self) -> Result<(), Error> {
        validate::graph(self)
    }
}

impl VertexInfo {
    /// Parses one vertex document. It does not validate.
    pub fn load(yaml: &str) -> Result<Self, Error> {
        yaml::load_vertex(yaml, "vertex info")
    }

    /// Validates, then writes the vertex document.
    pub fn dump(&self) -> Result<String, Error> {
        self.validate()?;
        yaml::dump_vertex(self)
    }

    /// The vertex document's file name, `<type>.vertex.yml`.
    pub fn file_name(&self) -> String {
        format!("{}.vertex.yml", self.r#type)
    }

    /// Checks the rules a reader relies on, and returns the first one broken.
    pub fn validate(&self) -> Result<(), Error> {
        validate::vertex(self)
    }
}

impl EdgeInfo {
    /// Parses one edge document. It does not validate.
    pub fn load(yaml: &str) -> Result<Self, Error> {
        yaml::load_edge(yaml, "edge info")
    }

    /// Validates, then writes the edge document.
    pub fn dump(&self) -> Result<String, Error> {
        self.validate()?;
        yaml::dump_edge(self)
    }

    /// `<src_type>_<edge_type>_<dst_type>`, which names the edge type's file and default prefix.
    pub fn triple(&self) -> String {
        format!("{}_{}_{}", self.src_type, self.edge_type, self.dst_type)
    }

    /// The edge document's file name, `<src_type>_<edge_type>_<dst_type>.edge.yml`.
    pub fn file_name(&self) -> String {
        format!("{}.edge.yml", self.triple())
    }

    /// Checks the rules a reader relies on, and returns the first one broken.
    pub fn validate(&self) -> Result<(), Error> {
        validate::edge(self)
    }
}

impl AdjListType {
    /// GraphAr's name for it, as `ordered_by_source`.
    pub fn as_str(self) -> &'static str {
        match self {
            AdjListType::UnorderedBySource => "unordered_by_source",
            AdjListType::OrderedBySource => "ordered_by_source",
            AdjListType::UnorderedByDest => "unordered_by_dest",
            AdjListType::OrderedByDest => "ordered_by_dest",
        }
    }

    /// Whether the edges are ordered within a partition: the document's `ordered`.
    pub fn ordered(self) -> bool {
        matches!(
            self,
            AdjListType::OrderedBySource | AdjListType::OrderedByDest
        )
    }

    /// Whether the partition key is the source: the document's `aligned_by: src`.
    pub fn by_source(self) -> bool {
        matches!(
            self,
            AdjListType::OrderedBySource | AdjListType::UnorderedBySource
        )
    }

    fn of(ordered: bool, by_source: bool) -> Self {
        match (ordered, by_source) {
            (true, true) => AdjListType::OrderedBySource,
            (true, false) => AdjListType::OrderedByDest,
            (false, true) => AdjListType::UnorderedBySource,
            (false, false) => AdjListType::UnorderedByDest,
        }
    }
}

impl fmt::Display for AdjListType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FileType {
    /// GraphAr's name for it, as `parquet`.
    pub fn as_str(self) -> &'static str {
        match self {
            FileType::Csv => "csv",
            FileType::Json => "json",
            FileType::Parquet => "parquet",
            FileType::Orc => "orc",
        }
    }
}

impl fmt::Display for FileType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for FileType {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        Ok(match s {
            "csv" => FileType::Csv,
            "json" => FileType::Json,
            "parquet" => FileType::Parquet,
            "orc" => FileType::Orc,
            _ => return Err(format!("unknown file type `{s}`")),
        })
    }
}

impl Cardinality {
    /// GraphAr's name for it, as `list`.
    pub fn as_str(self) -> &'static str {
        match self {
            Cardinality::Single => "single",
            Cardinality::List => "list",
            Cardinality::Set => "set",
        }
    }
}

impl fmt::Display for Cardinality {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Cardinality {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        Ok(match s {
            "single" => Cardinality::Single,
            "list" => Cardinality::List,
            "set" => Cardinality::Set,
            _ => return Err(format!("unknown cardinality `{s}`")),
        })
    }
}

impl fmt::Display for DataType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            DataType::Bool => "bool",
            DataType::Int32 => "int32",
            DataType::Int64 => "int64",
            DataType::Float => "float",
            DataType::Double => "double",
            DataType::String => "string",
            DataType::Date => "date",
            DataType::Timestamp => "timestamp",
            DataType::Time => "time",
            DataType::List(item) => return write!(f, "list<{item}>"),
            DataType::UserDefined(name) => name,
        };
        f.write_str(name)
    }
}

impl FromStr for DataType {
    type Err = String;
    /// Every name parses: one GraphAr does not define is [`DataType::UserDefined`], which
    /// [`GraphInfo::validate`] holds against the version's declared types.
    fn from_str(s: &str) -> Result<Self, String> {
        Ok(match s {
            "bool" => DataType::Bool,
            "int32" => DataType::Int32,
            "int64" => DataType::Int64,
            "float" => DataType::Float,
            "double" => DataType::Double,
            "string" => DataType::String,
            "date" => DataType::Date,
            "timestamp" => DataType::Timestamp,
            "time" => DataType::Time,
            "" => return Err("empty data type".to_owned()),
            _ => match s.strip_prefix("list<").and_then(|r| r.strip_suffix('>')) {
                Some(item) => DataType::List(Box::new(item.parse()?)),
                None => DataType::UserDefined(s.to_owned()),
            },
        })
    }
}

impl fmt::Display for InfoVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "gar/v{}", self.version)?;
        if !self.user_define_types.is_empty() {
            write!(f, " ({})", self.user_define_types.join(","))?;
        }
        Ok(())
    }
}

impl FromStr for InfoVersion {
    type Err = String;
    /// `gar/v1`, or `gar/v1 (a, b)`. GraphAr defines version 1 only, and any other is refused.
    fn from_str(s: &str) -> Result<Self, String> {
        let bad = || format!("unsupported version `{s}`: GraphAr defines `gar/v1`");
        let rest = s.trim().strip_prefix("gar/v").ok_or_else(bad)?;
        let digits = rest
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(rest.len());
        let version: u32 = rest[..digits].parse().map_err(|_| bad())?;
        if version != 1 {
            return Err(bad());
        }
        let rest = rest[digits..].trim_start();
        let user_define_types = if rest.is_empty() {
            Vec::new()
        } else {
            let list = rest
                .strip_prefix('(')
                .and_then(|r| r.strip_suffix(')'))
                .ok_or_else(bad)?;
            list.split(',')
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .map(str::to_owned)
                .collect()
        };
        Ok(InfoVersion {
            version,
            user_define_types,
        })
    }
}
