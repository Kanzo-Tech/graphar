//! **RDF in Apache GraphAr.** An RDF graph and the shapes that describe it (SHACL, ShEx) become a
//! GraphAr graph: a vertex table per class the shapes target, a column per property they declare,
//! an edge type per property whose values are subjects of another class, and one residual table
//! holding every triple the shapes do not cover. The RDF reading of those tables is an RML-Core
//! mapping, written beside them as `mapping.ttl`.
//!
//! - [`Schema`] is that reading: per vertex type its class, per column its predicate, object kind
//!   and cardinality, per edge type its predicate. A shape document makes one ([`shapes`]), and so
//!   does a host that already has a typed graph (fossil, from its program's graph type).
//! - [`mapping()`] is the mapping of a [`Schema`]: one triples map per vertex type, per edge type
//!   and per multi-valued or language-tagged column, and eight over the residual table. It reads no
//!   data, so a host can ask for it before writing.
//! - [`write()`] partitions the triples by the schema and writes the graph, the counts of
//!   `graphar-stats` in every vertex chunk and a Puffin file per type, then `mapping.ttl`, then the
//!   information files.
//! - [`read()`] is the mapping evaluated over the written tables, natively: the triples it reads
//!   are the triples [`write()`] was given (**Statement R**). The same holds through any RML
//!   processor that runs `mapping.ttl` on the tables; `graphar-bench` checks rudof's.
//!
//! **No triple is lost.** A literal goes to a typed column only if its lexical form is the one the
//! column gives back (`"01"^^xsd:integer` is not `"1"^^xsd:integer` in RDF, so it stays in the
//! residual table); a single-valued column with two values keeps neither; an object that is not a
//! subject of the edge's destination type stays a triple. The shapes decide how much of the graph
//! is typed, never whether it is kept.
//!
//! **Blocking.** [`write()`] groups the triples by subject in memory; the graph must fit.

mod mapping;
mod read;
mod schema;
pub mod shapes;
mod term;
mod write;

pub use mapping::mapping;
pub use read::read;
pub use schema::{
    Cardinality, Column, EdgeType, Error, Object, RESIDUAL, Schema, Storage, VertexType,
};
pub use write::{Options, Written, write};

/// `rdf:type`.
pub const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
/// `rdf:langString`, the datatype of a language-tagged string.
pub const RDF_LANG_STRING: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#langString";
/// The XSD namespace.
pub const XSD: &str = "http://www.w3.org/2001/XMLSchema#";
/// `xsd:string`, the datatype of a simple literal.
pub const XSD_STRING: &str = "http://www.w3.org/2001/XMLSchema#string";

/// The column of a vertex's IRI, the first of every vertex table.
pub const SUBJECT: &str = "subject";

/// The name of the document [`write()`] puts beside the information files.
pub const MAPPING_FILE: &str = "mapping.ttl";
