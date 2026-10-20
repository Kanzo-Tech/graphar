//! A [`Schema`](crate::Schema) from a shape document: SHACL ([`shacl`]) or ShEx ([`shex`]).
//!
//! Both read the same fragment, the one a table can hold. A shape becomes a vertex type when it
//! names its class (SHACL's `sh:targetClass`, or a shape that is itself a class; in ShEx, a
//! constraint that `rdf:type` is one IRI). Each property of it whose path is one IRI becomes:
//!
//! - an **edge type** when its values are instances of another shape's class (`sh:class`,
//!   `sh:node`, a ShEx shape reference);
//! - a **column** of IRIs (`sh:nodeKind sh:IRI`, `sh:class` of a class no shape targets, ShEx
//!   `nodeKind: iri`), of language-tagged strings (`rdf:langString`, `sh:languageIn`), or of
//!   literals of one datatype (`sh:datatype`).
//!
//! It is single-valued when its maximum is 1 and a set otherwise. Anything else a shape says
//! (`sh:or`, inverse and sequence paths, value ranges, patterns) types nothing: a shape here only
//! decides where a triple goes, and [`crate::write()`] refuses a graph with a triple that has no
//! place, which is the closed reading (`sh:closed`, ShEx `CLOSED`) of these shapes. A shape that
//! names no class targets nothing `write` reads.
//!
//! **Order.** Types are in the order of their classes' IRIs, and the columns and edge types of
//! each in the order of their predicates' IRIs: a function of the shapes, whichever language and
//! syntax states them, and not of the order a document happens to list them in. Of two shapes of
//! one class the one with the smaller label is read.
//!
//! **Names.** A type is named by its class's local name and a column or an edge type by its
//! predicate's, made a plain identifier; a name already taken gets `_2`, `_3`, … in that order.
//! The IRIs are the meaning and they are in the mapping, so two predicates with one local name
//! (`foaf:name`, `schema:name`) are two columns, `name` and `name_2`.

pub mod shacl;
pub mod shex;

use std::collections::HashSet;

use crate::SUBJECT;
use crate::schema::{Cardinality, Column, EdgeType, Object, Schema, VertexType};

/// What a property of a shape points at, before the shapes are matched to types.
#[derive(Clone, Debug)]
pub(crate) enum Target {
    /// A column of these objects.
    Column(Object),
    /// Instances of this class.
    Class(String),
    /// Conforming to this shape.
    Shape(String),
}

/// A shape as both decoders read it.
#[derive(Clone, Debug)]
pub(crate) struct Shape {
    pub id: String,
    pub class: Option<String>,
    pub properties: Vec<(String, Target, Cardinality)>,
}

/// The schema of `shapes`, in the order the module comment gives.
pub(crate) fn schema(shapes: &[Shape]) -> Schema {
    let mut typed: Vec<&Shape> = shapes.iter().filter(|s| s.class.is_some()).collect();
    typed.sort_by(|a, b| (&a.class, &a.id).cmp(&(&b.class, &b.id)));
    let mut seen_classes = HashSet::new();
    let typed: Vec<&Shape> = typed
        .into_iter()
        .filter(|s| seen_classes.insert(s.class.clone()))
        .collect();
    let mut labels = Names::new(&[]);
    let label_of: Vec<String> = typed
        .iter()
        .map(|s| labels.take(local(s.class.as_deref().expect("typed"))))
        .collect();
    let by_class = |c: &str| typed.iter().position(|s| s.class.as_deref() == Some(c));
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
                Target::Class(c) => by_class(c),
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
                // A class no shape targets: its instances are IRIs.
                (None, Target::Class(_)) => columns.push(Column {
                    name: names.take(local(predicate)),
                    predicate: predicate.clone(),
                    object: Object::Iri,
                    cardinality: *cardinality,
                }),
                // A shape with no class selects no rows, so its instances are IRIs too.
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
    schema
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
