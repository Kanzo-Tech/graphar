//! SHACL Core shapes graphs in Turtle (W3C Recommendation, 20 July 2017), the fragment the module
//! comment of [`crate::shapes`] names.

use std::collections::HashMap;

use oxrdf::{NamedOrBlankNode, Term, Triple};
use oxttl::TurtleParser;

use super::{Shape, Target};
use crate::schema::{Cardinality, Error, Object, Schema};
use crate::{RDF_LANG_STRING, RDF_TYPE};

const SH: &str = "http://www.w3.org/ns/shacl#";
const RDFS_CLASS: &str = "http://www.w3.org/2000/01/rdf-schema#Class";

/// The schema of a SHACL shapes graph written in Turtle.
///
/// # Errors
/// [`Error::Shapes`] when the document does not parse.
pub fn from_turtle(text: &str) -> Result<Schema, Error> {
    let triples = TurtleParser::new()
        .for_slice(text.as_bytes())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| Error::Shapes(e.to_string()))?;
    Ok(super::schema(&shapes(&triples)))
}

fn shapes(triples: &[Triple]) -> Vec<Shape> {
    let mut about: HashMap<&NamedOrBlankNode, Vec<&Triple>> = HashMap::new();
    for t in triples {
        about.entry(&t.subject).or_default().push(t);
    }
    let objects = |s: &NamedOrBlankNode, p: &str| -> Vec<&Term> {
        about
            .get(s)
            .into_iter()
            .flatten()
            .filter(|t| t.predicate.as_str() == p)
            .map(|t| &t.object)
            .collect()
    };
    let sh = |local: &str| format!("{SH}{local}");
    let iri = |t: &Term| match t {
        Term::NamedNode(n) => Some(n.as_str().to_owned()),
        _ => None,
    };
    let node = |t: &Term| match t {
        Term::NamedNode(n) => Some(NamedOrBlankNode::from(n.clone())),
        Term::BlankNode(b) => Some(NamedOrBlankNode::from(b.clone())),
        _ => None,
    };
    let id = |n: &NamedOrBlankNode| n.to_string();

    // Node shapes, in document order of their first triple.
    let mut order: Vec<&NamedOrBlankNode> = Vec::new();
    for t in triples {
        let node_shape = t.predicate.as_str() == RDF_TYPE
            && matches!(&t.object, Term::NamedNode(c) if c.as_str() == sh("NodeShape"));
        if node_shape && !order.contains(&&t.subject) {
            order.push(&t.subject);
        }
    }
    order
        .into_iter()
        .map(|s| {
            let class = objects(s, &sh("targetClass"))
                .into_iter()
                .find_map(iri)
                .or_else(|| {
                    let implicit = objects(s, RDF_TYPE)
                        .iter()
                        .any(|c| iri(c).as_deref() == Some(RDFS_CLASS));
                    match (implicit, s) {
                        (true, NamedOrBlankNode::NamedNode(n)) => Some(n.as_str().to_owned()),
                        _ => None,
                    }
                });
            let properties = objects(s, &sh("property"))
                .into_iter()
                .filter_map(node)
                .filter_map(|p| {
                    let path = objects(&p, &sh("path")).into_iter().find_map(iri)?;
                    let one = |local: &str| objects(&p, &sh(local)).into_iter().next();
                    let target = if let Some(dt) = one("datatype").and_then(iri) {
                        Target::Column(if dt == RDF_LANG_STRING {
                            Object::LangString
                        } else {
                            Object::Literal(dt)
                        })
                    } else if one("languageIn").is_some() {
                        Target::Column(Object::LangString)
                    } else if let Some(class) = one("class").and_then(iri) {
                        Target::Class(class)
                    } else if let Some(shape) = one("node").and_then(node) {
                        Target::Shape(id(&shape))
                    } else if one("nodeKind").and_then(iri).as_deref() == Some(&sh("IRI")) {
                        Target::Column(Object::Iri)
                    } else {
                        return None;
                    };
                    let single = one("maxCount").is_some_and(|m| match m {
                        Term::Literal(l) => l.value() == "1",
                        _ => false,
                    });
                    let cardinality = if single {
                        Cardinality::Single
                    } else {
                        Cardinality::Set
                    };
                    Some((path, target, cardinality))
                })
                .collect();
            Shape {
                id: id(s),
                class,
                properties,
            }
        })
        .collect()
}
