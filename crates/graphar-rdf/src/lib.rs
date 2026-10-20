//! **RDF in Apache GraphAr.** An RDF graph that conforms to the shapes describing it (SHACL,
//! ShEx) becomes a GraphAr graph: a vertex table per class the shapes target, a column per property
//! they declare, and an edge type per property whose values are subjects of another class. The RDF
//! reading of those tables is a plan in the mapping algebra of Min Oo and Hartig, written beside
//! them in RML-Core as `mapping.ttl` and in R2RML as `mapping.r2rml.ttl`.
//!
//! - [`Schema`] is that reading: per vertex type its class, per column its predicate, object kind
//!   and cardinality, per edge type its predicate. A shape document makes one ([`shapes`]), and so
//!   does a host that already has a typed graph (fossil, from its program's graph type).
//! - [`plan()`] is the reading of a [`Schema`] in the algebra: a union of [`Map`]s, one per vertex
//!   type, per edge type and per multi-valued or language-tagged column. It reads no data, so a
//!   host can ask for it, or for [`mapping()`], its RML-Core, before writing. [`rml()`] and
//!   [`r2rml()`] serialize it; R2RML needs the languages a column holds, which only the data has.
//! - [`write()`] places every triple by the schema and writes the graph, the counts of
//!   `graphar-stats` in every vertex chunk and a Puffin file per type of several chunks, then the
//!   two mappings, then the information files.
//! - [`read()`] is the plan evaluated over the written tables, natively: the triples it reads are
//!   the triples [`write()`] was given, up to the canonical lexical form of a literal
//!   (**Statement R**). The same holds through a processor that runs either mapping on the tables;
//!   `conformance/rml` runs Morph-KGC and RMLMapper on both, and checks that AlgeMapLoom translates
//!   `mapping.ttl` into the same plan.
//!
//! **Closed.** Only a conforming graph is written: every subject an IRI typed by exactly one
//! targeted class, every other triple a value of one of its type's columns or an edge to a row of
//! the destination type, and no single-valued column with two values. Anything else is refused
//! with the triple and the reason ([`Error::Unplaced`]) before a file is written. A graph the
//! shapes describe only in part has no representation here.
//!
//! **Blocking.** [`write()`] groups the triples by subject in memory; the graph must fit.

mod mapping;
mod plan;
mod read;
mod schema;
pub mod shapes;
mod term;
mod write;

pub use mapping::{mapping, r2rml, rml};
pub use plan::{Expr, Map, Plan, Source, plan};
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

/// The name of the RML-Core document [`write()`] puts beside the information files.
pub const MAPPING_FILE: &str = "mapping.ttl";
/// The name of the R2RML document [`write()`] puts beside it.
pub const R2RML_FILE: &str = "mapping.r2rml.ttl";
