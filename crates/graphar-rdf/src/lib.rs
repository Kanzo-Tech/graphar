//! **RDF in Apache GraphAr.** An RDF graph that conforms to the shapes describing it (SHACL,
//! ShEx) becomes a GraphAr graph: a vertex table per class the shapes target, a column per property
//! they declare, and an edge type per property whose values are subjects of another class. The RDF
//! reading of those tables is an RML-Core mapping, written beside them as `mapping.ttl`.
//!
//! - [`Schema`] is that reading: per vertex type its class, per column its predicate, object kind
//!   and cardinality, per edge type its predicate. A shape document makes one ([`shapes`]), and so
//!   does a host that already has a typed graph (fossil, from its program's graph type).
//! - [`mapping()`] is the mapping of a [`Schema`]: one triples map per vertex type, per edge type
//!   and per multi-valued or language-tagged column. It reads no data, so a host can ask for it
//!   before writing.
//! - [`write()`] places every triple by the schema and writes the graph, the counts of
//!   `graphar-stats` in every vertex chunk and a Puffin file per type of several chunks, then
//!   `mapping.ttl`, then the information files.
//! - [`read()`] is the mapping evaluated over the written tables, natively: the triples it reads
//!   are the triples [`write()`] was given, up to the canonical lexical form of a literal
//!   (**Statement R**). The same holds through an RML processor that runs `mapping.ttl` on the
//!   tables; `conformance/rml` runs Morph-KGC and RMLMapper.
//!
//! **Closed.** Only a conforming graph is written: every subject an IRI typed by exactly one
//! targeted class, every other triple a value of one of its type's columns or an edge to a row of
//! the destination type, and no single-valued column with two values. Anything else is refused
//! with the triple and the reason ([`Error::Unplaced`]) before a file is written. A graph the
//! shapes describe only in part has no representation here.
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
pub use schema::{Cardinality, Column, EdgeType, Error, Object, Schema, Storage, VertexType};
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
