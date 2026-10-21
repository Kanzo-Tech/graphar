//! A [`Schema`] from a shape document: SHACL ([`shacl`]) or ShEx ([`shex`]), and what validates
//! a graph against the document ([`Shapes`]).
//!
//! **A shape is a type.** Every shape named by an IRI becomes a vertex type, and a node belongs to
//! the type of the shape it validates against, whatever its triples claim: `rdf:type` is a predicate
//! like any other. A shape that fixes it to one class (`rdf:type [c]` with exactly one value; in
//! SHACL a property shape on `rdf:type` with `sh:hasValue c` and `sh:maxCount 1`, or `sh:in (c)`
//! with `sh:minCount 1` and `sh:maxCount 1`) has that class, which the mapping writes as the
//! triples map's `rml:class` and no table stores. Targets (`sh:targetClass`, …) type nothing. Each
//! property of a shape whose path is one IRI becomes:
//!
//! - an **edge type** when its values must validate against another shape (`sh:node`, a ShEx
//!   shape reference): the destination is a type, not a class;
//! - a **column** of IRIs (`sh:nodeKind sh:IRI`, `sh:class`, a value set of IRIs, a reference to a
//!   shape no IRI names), of language-tagged strings (`rdf:langString`, `sh:languageIn`), or of
//!   literals of one datatype (`sh:datatype`).
//!
//! It is single-valued when its maximum is 1 and a set otherwise. Anything else a shape says
//! (`sh:or`, inverse and sequence paths, value ranges, patterns, minimums) types nothing: a shape
//! here only decides where a triple goes, and [`crate::write()`] refuses a graph with a triple
//! that has no place, which is the closed reading (`sh:closed`, ShEx `CLOSED`) of these shapes.
//!
//! **Order.** Types are in the order of their shapes' IRIs, and the columns and edge types of each
//! in the order of their predicates' IRIs: a function of the shapes, whichever language and syntax
//! states them, and not of the order a document happens to list them in.
//!
//! **Names.** A type is named by its shape's local name and a column or an edge type by its
//! predicate's, made a plain identifier; a name already taken gets `_2`, `_3`, … in that order.
//! The IRIs are the meaning and they are in the mapping, so two predicates with one local name
//! (`foaf:name`, `schema:name`) are two columns, `name` and `name_2`.

pub mod shacl;
pub mod shex;

use std::collections::HashSet;

use crate::SUBJECT;
use crate::schema::{Cardinality, Column, EdgeType, Object, Schema, VertexType};
use crate::validate::Document;

/// What a property of a shape points at, before the shapes are matched to types.
#[derive(Clone, Debug)]
pub(crate) enum Target {
    /// A column of these objects.
    Column(Object),
    /// Values that validate against this shape.
    Shape(String),
}

/// A shape as both decoders read it.
#[derive(Clone, Debug)]
pub(crate) struct Shape {
    pub id: String,
    /// The one `rdf:type` the shape fixes.
    pub class: Option<String>,
    pub properties: Vec<(String, Target, Cardinality)>,
}

/// A shape document: the [`Schema`] it describes, and the shapes themselves, which
/// [`crate::Typing::Validate`] validates a graph against.
#[derive(Debug)]
pub struct Shapes {
    schema: Schema,
    /// The shape of each vertex type, by its IRI, in the schema's order.
    pub(crate) shapes: Vec<String>,
    pub(crate) document: Document,
}

impl Shapes {
    /// The schema the shapes describe.
    #[must_use]
    pub fn schema(&self) -> &Schema {
        &self.schema
    }

    /// The IRI of the shape a vertex type is, by its label.
    #[must_use]
    pub fn shape(&self, label: &str) -> Option<&str> {
        let v = self.schema.vertices.iter().position(|v| v.label == label)?;
        Some(&self.shapes[v])
    }
}

/// The shapes of a document, read off it.
pub(crate) fn read(shapes: &[Shape], document: Document) -> Shapes {
    let (schema, ids) = schema(shapes);
    Shapes {
        schema,
        shapes: ids,
        document,
    }
}

/// The schema of `shapes`, in the order the module comment gives, and the shape of each type.
fn schema(shapes: &[Shape]) -> (Schema, Vec<String>) {
    let mut typed: Vec<&Shape> = shapes.iter().filter(|s| !s.id.starts_with("_:")).collect();
    typed.sort_by(|a, b| a.id.cmp(&b.id));
    typed.dedup_by(|a, b| a.id == b.id);
    let mut labels = Names::new(&[]);
    let label_of: Vec<String> = typed.iter().map(|s| labels.take(local(&s.id))).collect();
    let by_shape = |id: &str| typed.iter().position(|s| s.id == id);

    let mut schema = Schema::default();
    for (i, shape) in typed.iter().enumerate() {
        let mut names = Names::new(&[SUBJECT]);
        let mut edge_names = Names::new(&[]);
        let mut predicates = HashSet::new();
        let mut columns = Vec::new();
        let mut properties: Vec<_> = shape.properties.iter().collect();
        properties.sort_by(|a, b| a.0.cmp(&b.0));
        for (predicate, target, cardinality) in properties {
            if !predicates.insert(predicate.clone()) {
                continue;
            }
            let destination = match target {
                Target::Shape(id) => by_shape(id),
                Target::Column(_) => None,
            };
            match (destination, target) {
                (Some(d), _) => schema.edges.push(EdgeType {
                    label: edge_names.take(local(predicate)),
                    source: label_of[i].clone(),
                    destination: label_of[d].clone(),
                    predicate: predicate.clone(),
                }),
                (None, Target::Column(object)) => columns.push(Column {
                    name: names.take(local(predicate)),
                    predicate: predicate.clone(),
                    object: object.clone(),
                    cardinality: *cardinality,
                }),
                // A shape no IRI names is no type: its values are IRIs.
                (None, Target::Shape(_)) => columns.push(Column {
                    name: names.take(local(predicate)),
                    predicate: predicate.clone(),
                    object: Object::Iri,
                    cardinality: *cardinality,
                }),
            }
        }
        schema.vertices.push(VertexType {
            label: label_of[i].clone(),
            class: shape.class.clone(),
            columns,
        });
    }
    (schema, typed.iter().map(|s| s.id.clone()).collect())
}

/// An IRI's local name, as a plain identifier.
fn local(iri: &str) -> String {
    let tail = iri
        .rsplit(['#', '/', ':'])
        .find(|s| !s.is_empty())
        .unwrap_or("x");
    let mut name: String = tail
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    if !name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_') {
        name.insert(0, '_');
    }
    name
}

/// Names handed out once each.
struct Names(HashSet<String>);

impl Names {
    fn new(reserved: &[&str]) -> Self {
        Self(reserved.iter().map(|s| (*s).to_owned()).collect())
    }

    fn take(&mut self, name: String) -> String {
        let name = if name.starts_with("_graphAr") {
            format!("_{name}")
        } else {
            name
        };
        if self.0.insert(name.clone()) {
            return name;
        }
        (2..)
            .map(|n| format!("{name}_{n}"))
            .find(|n| self.0.insert(n.clone()))
            .expect("a free name")
    }
}
