//! The RDF reading of a GraphAr graph, as data: what each table, column and edge type means.

use std::collections::HashMap;

use arrow_schema::{DataType as Arrow, Field, Fields};
use graphar_info::{
    AdjListType, AdjacentList, DataType, EdgeInfo, FileType, GraphInfo, InfoVersion, Property,
    PropertyGroup, VertexInfo,
};
use graphar_parquet::UserTypes;

use crate::term::storage_of;
use crate::{RDF_LANG_STRING, SUBJECT};

/// The user-defined type of a language-tagged string: `gar/v1 (langString)`.
pub(crate) const LANG_STRING: &str = "langString";

/// The RDF reading of a GraphAr graph.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Schema {
    /// The vertex types, each a table of subjects.
    pub vertices: Vec<VertexType>,
    /// The edge types, each a predicate between the subjects of two vertex types.
    pub edges: Vec<EdgeType>,
    /// Triples about the IRIs the schema names (a class's `rdfs:label`, say), written in the
    /// mapping as RML-Core §3.2 allows.
    pub annotations: Vec<oxrdf::Triple>,
}

/// A vertex type: a table whose rows are subjects, the `subject` column their IRIs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VertexType {
    /// The table's name, which is the GraphAr vertex type.
    pub label: String,
    /// The class every row is an instance of, by the one `rdf:type` triple each subject has. A
    /// type without one is targeted by nothing [`crate::write()`] reads, and holds no row.
    pub class: Option<String>,
    /// The columns beside `subject`, in table order.
    pub columns: Vec<Column>,
}

/// A column of a vertex type: the objects of one predicate for each subject.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Column {
    /// The column's name, the GraphAr property.
    pub name: String,
    /// The predicate.
    pub predicate: String,
    /// What its objects are.
    pub object: Object,
    /// How many objects one subject has in it.
    pub cardinality: Cardinality,
}

/// What the objects of a column are.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Object {
    /// IRIs, as text.
    Iri,
    /// Language-tagged strings: `STRUCT<value, lang>`, GraphAr's user-defined `langString`.
    LangString,
    /// Literals of this datatype, stored as [`Storage`] says.
    Literal(String),
}

/// How many objects of a column one subject has.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Cardinality {
    /// At most one: a value, or null.
    Single,
    /// Any number: a GraphAr `set`, a list of distinct values.
    Set,
}

/// An edge type: a predicate whose subject is a vertex of `source` and whose object a vertex of
/// `destination`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EdgeType {
    /// The GraphAr edge type.
    pub label: String,
    /// The source vertex type.
    pub source: String,
    /// The destination vertex type.
    pub destination: String,
    /// The predicate.
    pub predicate: String,
}

/// The column type of a column.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Storage {
    /// `string`.
    String,
    /// `int64`.
    Int64,
    /// `int32`.
    Int32,
    /// `bool`.
    Bool,
    /// `date`.
    Date,
    /// The user-defined `langString`.
    LangString,
}

/// Why a schema is not one a graph can be written or read by.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// One IRI that would name two things in the RDF image (A2).
    #[error("`{iri}` names both `{first}` and `{second}`")]
    AmbiguousIri {
        /// The IRI.
        iri: String,
        /// The first thing it names.
        first: String,
        /// The second.
        second: String,
    },
    /// A name used twice, or reserved.
    #[error("`{0}` is used twice or is reserved")]
    Name(String),
    /// An edge type whose end is not a vertex type.
    #[error("edge `{0}` names a vertex type that does not exist")]
    End(String),
    /// A text that is not an IRI.
    #[error("`{0}` is not an IRI")]
    NotAnIri(String),
    /// A shape document that does not parse.
    #[error("the shapes: {0}")]
    Shapes(String),
    /// The triples do not parse.
    #[error("the triples: {0}")]
    Syntax(String),
    /// A triple the schema has no place for: the graph does not conform to the closed reading of
    /// its shapes, and nothing was written.
    #[error("{triple} does not conform: {reason}")]
    Unplaced {
        /// The triple, in N-Triples.
        triple: String,
        /// Why it has no place.
        reason: String,
    },
    /// Writing or reading the GraphAr files.
    #[error(transparent)]
    Parquet(#[from] graphar_parquet::Error),
    /// The information files.
    #[error(transparent)]
    Info(#[from] graphar_info::Error),
    /// Counting what is written.
    #[error(transparent)]
    Stats(#[from] graphar_stats::Error),
    /// Assembling a batch.
    #[error(transparent)]
    Arrow(#[from] arrow_schema::ArrowError),
    /// The store.
    #[error(transparent)]
    Store(#[from] object_store::Error),
    /// A table that does not hold what its information file says.
    #[error("{0}")]
    Invalid(String),
}

impl Column {
    /// The column type that holds this column's objects.
    #[must_use]
    pub fn storage(&self) -> Storage {
        match &self.object {
            Object::Iri => Storage::String,
            Object::LangString => Storage::LangString,
            Object::Literal(dt) if dt == RDF_LANG_STRING => Storage::LangString,
            Object::Literal(dt) => storage_of(dt),
        }
    }
}

impl Storage {
    /// The GraphAr data type.
    #[must_use]
    pub fn data_type(self) -> DataType {
        match self {
            Self::String => DataType::String,
            Self::Int64 => DataType::Int64,
            Self::Int32 => DataType::Int32,
            Self::Bool => DataType::Bool,
            Self::Date => DataType::Date,
            Self::LangString => DataType::UserDefined(LANG_STRING.to_owned()),
        }
    }
}

/// The Arrow type of `langString`: `STRUCT<value: STRING, lang: STRING>`.
#[must_use]
pub fn lang_string_type() -> Arrow {
    Arrow::Struct(Fields::from(vec![
        Field::new("value", Arrow::Utf8, false),
        Field::new("lang", Arrow::Utf8, false),
    ]))
}

/// The user-defined types a graph of this crate declares, with their Arrow types.
#[must_use]
pub fn user_types() -> UserTypes {
    UserTypes::from([(LANG_STRING.to_owned(), lang_string_type())])
}

impl Schema {
    /// Whether the schema can be read unambiguously (A2): two vertex types do not share a class,
    /// two columns of a type do not share a predicate, and no edge type's predicate is a column
    /// predicate of its source. Names are unique, none is reserved, and every IRI is one.
    ///
    /// # Errors
    /// The first thing that is not so.
    pub fn check(&self) -> Result<(), Error> {
        let ambiguous = |iri: &str, first: &str, second: &str| Error::AmbiguousIri {
            iri: iri.to_owned(),
            first: first.to_owned(),
            second: second.to_owned(),
        };
        let iri = |text: &str| -> Result<(), Error> {
            oxrdf::NamedNode::new(text)
                .map(|_| ())
                .map_err(|_| Error::NotAnIri(text.to_owned()))
        };
        let mut labels: HashMap<&str, ()> = HashMap::new();
        let mut classes: HashMap<&str, &str> = HashMap::new();
        for v in &self.vertices {
            if !named(&v.label) || labels.insert(&v.label, ()).is_some() {
                return Err(Error::Name(v.label.clone()));
            }
            if let Some(class) = &v.class {
                iri(class)?;
                if let Some(other) = classes.insert(class, &v.label) {
                    return Err(ambiguous(class, other, &v.label));
                }
            }
            let mut names: HashMap<&str, ()> = HashMap::new();
            let mut predicates: HashMap<&str, String> = HashMap::new();
            for c in &v.columns {
                if c.name == SUBJECT
                    || c.name.starts_with("_graphAr")
                    || !named(&c.name)
                    || names.insert(&c.name, ()).is_some()
                {
                    return Err(Error::Name(format!("{}.{}", v.label, c.name)));
                }
                iri(&c.predicate)?;
                if let Object::Literal(dt) = &c.object {
                    iri(dt)?;
                }
                let at = format!("{}.{}", v.label, c.name);
                if let Some(other) = predicates.insert(&c.predicate, at.clone()) {
                    return Err(ambiguous(&c.predicate, &other, &at));
                }
            }
            for e in self.edges.iter().filter(|e| e.source == v.label) {
                if let Some(other) = predicates.get(e.predicate.as_str()) {
                    return Err(ambiguous(&e.predicate, other, &e.triple()));
                }
            }
        }
        let mut triples: HashMap<String, ()> = HashMap::new();
        for e in &self.edges {
            iri(&e.predicate)?;
            if !named(&e.label) || triples.insert(e.triple(), ()).is_some() {
                return Err(Error::Name(e.triple()));
            }
            if !labels.contains_key(e.source.as_str())
                || !labels.contains_key(e.destination.as_str())
            {
                return Err(Error::End(e.triple()));
            }
        }
        Ok(())
    }

    /// The vertex type named `label`.
    #[must_use]
    pub fn vertex(&self, label: &str) -> Option<&VertexType> {
        self.vertices.iter().find(|v| v.label == label)
    }

    /// The information files of the graph [`crate::write()`] writes: a vertex type per
    /// [`VertexType`], an edge type per [`EdgeType`], each stored as one
    /// property group, and adjacency lists ordered by source, every chunk `chunk_size` rows.
    #[must_use]
    pub fn graph_info(&self, name: &str, chunk_size: u64) -> GraphInfo {
        let tagged = self
            .vertices
            .iter()
            .flat_map(|v| &v.columns)
            .any(|c| c.storage() == Storage::LangString);
        let version = InfoVersion {
            version: 1,
            user_define_types: if tagged {
                vec![LANG_STRING.to_owned()]
            } else {
                Vec::new()
            },
        };
        let property = |name: &str, data_type, primary: bool, nullable: bool, set: bool| Property {
            name: name.to_owned(),
            data_type,
            is_primary: primary,
            is_nullable: nullable,
            cardinality: if set {
                graphar_info::Cardinality::Set
            } else {
                graphar_info::Cardinality::Single
            },
        };
        let vertex = |label: &str, properties: Vec<Property>| VertexInfo {
            r#type: label.to_owned(),
            chunk_size,
            prefix: format!("vertex/{label}/"),
            labels: Vec::new(),
            property_groups: vec![PropertyGroup {
                properties,
                file_type: FileType::Parquet,
                prefix: "properties/".to_owned(),
            }],
            version: version.clone(),
        };
        let vertices: Vec<VertexInfo> = self
            .vertices
            .iter()
            .map(|v| {
                let mut properties = vec![property(SUBJECT, DataType::String, true, false, false)];
                properties.extend(v.columns.iter().map(|c| {
                    property(
                        &c.name,
                        c.storage().data_type(),
                        false,
                        true,
                        c.cardinality == Cardinality::Set,
                    )
                }));
                vertex(&v.label, properties)
            })
            .collect();
        let edges = self
            .edges
            .iter()
            .map(|e| EdgeInfo {
                src_type: e.source.clone(),
                edge_type: e.label.clone(),
                dst_type: e.destination.clone(),
                chunk_size,
                src_chunk_size: chunk_size,
                dst_chunk_size: chunk_size,
                directed: true,
                prefix: format!("edge/{}/", e.triple()),
                adj_lists: vec![AdjacentList {
                    r#type: AdjListType::OrderedBySource,
                    file_type: FileType::Parquet,
                    prefix: "ordered_by_source/".to_owned(),
                }],
                property_groups: Vec::new(),
                version: version.clone(),
            })
            .collect();
        GraphInfo {
            name: name.to_owned(),
            prefix: None,
            vertices,
            edges,
            labels: Vec::new(),
            version,
            extra_info: Vec::new(),
        }
    }
}

impl EdgeType {
    /// `<source>_<label>_<destination>`, GraphAr's name for the edge type and its table.
    #[must_use]
    pub fn triple(&self) -> String {
        format!("{}_{}_{}", self.source, self.label, self.destination)
    }
}

/// Whether a name can name a table, a column and a path segment: not empty, no `/`, no control
/// character. Any other is quoted wherever SQL reads it.
fn named(name: &str) -> bool {
    !name.is_empty() && !name.chars().any(|c| c == '/' || c.is_control())
}

/// Whether a name is a plain SQL identifier and a path segment: an ASCII letter or `_`, then
/// letters, digits and `_`.
pub(crate) fn plain(name: &str) -> bool {
    name.chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}
