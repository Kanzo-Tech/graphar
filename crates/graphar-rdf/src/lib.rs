//! **RDF in Apache GraphAr.** An RDF graph that conforms to the shapes describing it (SHACL,
//! ShEx) becomes a GraphAr graph: a vertex table per shape, holding the subjects that validate
//! against it, a column per property it declares, and an edge type per property whose values are
//! nodes of another shape. `rdf:type` is a predicate like any other. The RDF
//! reading of those tables is a plan in the mapping algebra of Min Oo and Hartig, written beside
//! them in RML-Core as `mapping.ttl` and in R2RML as `mapping.r2rml.ttl`.
//!
//! - [`Schema`] is that reading: per vertex type its shape and the class the shape fixes, if any,
//!   per column its predicate, object kind and cardinality, per edge type its predicate. A shape
//!   document makes one ([`shapes`]), and so does a host that already has a typed graph (fossil,
//!   from its program's graph type), whose types have no shape.
//! - [`plan()`] is the reading of a [`Schema`] in the algebra: a union of [`Map`]s, one per vertex
//!   type, per edge type and per multi-valued or language-tagged column. It reads no data, so a
//!   host can ask for it, or for [`mapping()`], its RML-Core, before writing. [`rml()`] and
//!   [`r2rml()`] serialize it; R2RML needs the languages a column holds, which only the data has.
//! - [`Subject`] is one subject's triples with its vertex type: the type travels in the stream,
//!   because whoever produces the triples knows which shape it produced each subject for.
//!   [`by_class()`] types a stream of triples grouped by subject by their `rdf:type`, and
//!   [`grouped()`] groups triples held in memory.
//! - [`write()`] streams the subjects, places every triple by the schema and writes the graph,
//!   each chunk one row group whose Parquet `Statistics` carry its distinct counts, then the two
//!   mappings, then the shape document as it was read, then the information files.
//! - [`read()`] is the plan evaluated over the written tables, natively: the triples it reads are
//!   the triples [`write()`] was given, up to the canonical lexical form of a literal
//!   (**Statement R**). The same holds through a processor that runs either mapping on the tables;
//!   `conformance/rml` runs Morph-KGC and RMLMapper on both, and checks that AlgeMapLoom translates
//!   `mapping.ttl` into the same plan.
//!
//! **Closed.** Only a conforming graph is written: every subject an IRI of exactly one type,
//! every triple a value of one of its type's columns, the class its shape fixes, or an edge to a row of
//! the destination type, and no single-valued column with two values. Anything else is refused
//! with the triple and the reason ([`Error::Unplaced`]), or the subject and the types it was given
//! ([`Error::Typing`]), and the graph is not committed: its information files are written last,
//! and only when every triple has its place. A graph the shapes describe only in part has no
//! representation here.
//!
//! **Validation is not here.** The producer types each subject, and placing its triples checks
//! the structure of its shape (its columns, their value types and multiplicity, its references).
//! Validating the graph against the shapes is a validator's: rudof's, over the written tables
//! through `mapping.r2rml.ttl`.
//!
//! **Streaming.** [`write()`] takes the subjects one at a time, and checks that none comes twice
//! ([`Error::Ungrouped`]). Each subject's row is the next of its type, and a chunk is written as
//! soon as it fills. What it holds is what GraphAr's layout forces: every subject's IRI with its
//! type and row, because an adjacency list holds the destination's row and an edge may name its
//! destination before the destination's own triples; per vertex type, the open chunk of at most
//! `chunk_size` rows; per edge type, the edges from a chunk of sources until the chunk is written
//! and every destination of them has arrived; and one subject's triples. Memory is
//! O(subjects + types × `chunk_size`) when every destination arrives before the edges to it, and
//! each forward reference holds the part it is in, up to every edge held as a pair of rows. A
//! refused stream leaves its full chunks and their parts behind, uncommitted ([`write()`]).

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
pub use write::{Options, Subject, Written, by_class, grouped, write};

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
/// The name [`write()`] saves a SHACL shapes graph in Turtle as, beside the mappings.
pub const SHACL_FILE: &str = "shapes.ttl";
/// The name [`write()`] saves a ShEx schema in ShExC as.
pub const SHEXC_FILE: &str = "shapes.shex";
/// The name [`write()`] saves a ShEx schema in ShExJ as.
pub const SHEXJ_FILE: &str = "shapes.shexj";
/// The graph document's `extra_info` key whose value is [`MAPPING_FILE`]: a reader that finds it
/// reads the graph as RDF through that mapping.
pub const MAPPING_KEY: &str = "mapping";
/// The graph document's `extra_info` key whose value is the name of the shape document saved
/// beside the graph, one of the three above.
pub const SHAPES_KEY: &str = "shapes";
/// `dct:conformsTo`: the shape the subjects of a vertex type's triples map conform to.
pub const DCT_CONFORMS_TO: &str = "http://purl.org/dc/terms/conformsTo";
