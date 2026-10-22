//! SHACL Core shapes graphs in Turtle (W3C Recommendation, 20 July 2017), read by rudof: its
//! `rudof_rdf` parses the Turtle and `shacl`'s `ShaclParser` turns the graph into shapes. This
//! module only reads the fragment the module comment of [`crate::shapes`] names off them.

use rudof_rdf::rdf_core::term::Object as Node;
use rudof_rdf::rdf_core::{RDFFormat, SHACLPath};
use rudof_rdf::rdf_impl::{OxigraphInMemory, ReaderMode};
use shacl::ast::{ASTComponent, ASTSchema, ASTShape};
use shacl::ir::{IRSchema, RecursionSemantics};
use shacl::rdf::ShaclParser;
use shacl::types::{NodeKind, Value};

use super::{Shape, Shapes, Syntax, Target};
use crate::schema::{Cardinality, Error, Object};
use crate::validate::Document;
use crate::{RDF_LANG_STRING, RDF_TYPE};

/// The shapes of a SHACL shapes graph written in Turtle.
///
/// # Errors
/// [`Error::Shapes`] when the document does not parse or rudof refuses the shapes in it.
pub fn from_turtle(text: &str) -> Result<Shapes, Error> {
    let graph = OxigraphInMemory::from_str(text, &RDFFormat::Turtle, None, &ReaderMode::Strict)
        .map_err(|e| Error::Shapes(e.to_string()))?;
    let schema = ShaclParser::new(graph)
        .parse()
        .map_err(|e| Error::Shapes(e.to_string()))?;
    // The greatest fixpoint: a node validates against a recursive shape unless something refutes
    // it, which is the maximal typing (`crate::Typing::Validate`).
    let compiled = IRSchema::compile_with_recursion(&schema, RecursionSemantics::Gfp)
        .map_err(|e| Error::Shapes(e.to_string()))?;
    Ok(super::read(
        &shapes(&schema)?,
        Document::Shacl(Box::new(compiled)),
        Syntax::ShaclTurtle,
        text,
    ))
}

fn shapes(schema: &ASTSchema) -> Result<Vec<Shape>, Error> {
    schema
        .iter()
        .filter_map(|(id, shape)| match shape {
            ASTShape::NodeShape(node) => Some((id, node)),
            ASTShape::PropertyShape(_) => None,
        })
        .map(|(id, node)| {
            let mut class = None;
            let mut properties = Vec::new();
            for p in node
                .property_shapes()
                .iter()
                .filter_map(|p| match schema.get_shape(p) {
                    Some(ASTShape::PropertyShape(p)) => Some(p),
                    _ => None,
                })
            {
                let SHACLPath::Predicate { pred } = p.path() else {
                    continue;
                };
                let components = p.components();
                let max = components.iter().find_map(|c| match c {
                    ASTComponent::MaxCount(n) => Some(*n),
                    _ => None,
                });
                if max == Some(0) {
                    return Err(super::forbidden(&label(id), pred.as_str()));
                }
                if pred.as_str() == RDF_TYPE
                    && let Some(c) = fixed(components)
                {
                    class.get_or_insert(c);
                    continue;
                }
                let Some(target) = target(components) else {
                    continue;
                };
                let cardinality = if max == Some(1) {
                    Cardinality::Single
                } else {
                    Cardinality::Set
                };
                properties.push((pred.as_str().to_owned(), target, cardinality));
            }
            Ok(Shape {
                id: label(id),
                class,
                properties,
            })
        })
        .collect()
}

/// The one class a property shape on `rdf:type` fixes: `sh:hasValue c` with `sh:maxCount 1`, or
/// `sh:in (c)` with `sh:minCount 1` and `sh:maxCount 1` — exactly one value, and it is `c`.
fn fixed(components: &[ASTComponent]) -> Option<String> {
    let has = |wanted: &ASTComponent| components.contains(wanted);
    if !has(&ASTComponent::MaxCount(1)) {
        return None;
    }
    let iri = |v: &Value| match v {
        Value::Iri(i) => i.get_iri().ok().map(|i| i.as_str().to_owned()),
        Value::Literal(_) => None,
    };
    components.iter().find_map(|c| match c {
        ASTComponent::HasValue(v) => iri(v),
        ASTComponent::In(values) if values.len() == 1 && has(&ASTComponent::MinCount(1)) => {
            iri(&values[0])
        }
        _ => None,
    })
}

/// What a property shape's values are, by the first of `sh:datatype`, `sh:languageIn`,
/// `sh:node`, and an IRI by `sh:class`, `sh:nodeKind sh:IRI`, `sh:hasValue` or `sh:in` of IRIs it
/// has. `sh:class` is not a reference: it asks for a class triple, not a shape.
fn target(components: &[ASTComponent]) -> Option<Target> {
    let find = |f: fn(&ASTComponent) -> Option<Target>| components.iter().find_map(f);
    find(|c| match c {
        ASTComponent::Datatype(dt) => {
            let dt = dt.get_iri().ok()?;
            Some(Target::Column(if dt.as_str() == RDF_LANG_STRING {
                Object::LangString
            } else {
                Object::Literal(dt.as_str().to_owned())
            }))
        }
        _ => None,
    })
    .or_else(|| {
        find(|c| match c {
            ASTComponent::LanguageIn(_) => Some(Target::Column(Object::LangString)),
            _ => None,
        })
    })
    .or_else(|| {
        find(|c| match c {
            ASTComponent::Node(shape) => Some(Target::Shape(label(shape))),
            _ => None,
        })
    })
    .or_else(|| {
        find(|c| match c {
            ASTComponent::Class(Node::Iri(_)) | ASTComponent::NodeKind(NodeKind::Iri) => {
                Some(Target::Column(Object::Iri))
            }
            ASTComponent::HasValue(Value::Iri(_)) => Some(Target::Column(Object::Iri)),
            ASTComponent::In(values)
                if !values.is_empty() && values.iter().all(|v| matches!(v, Value::Iri(_))) =>
            {
                Some(Target::Column(Object::Iri))
            }
            _ => None,
        })
    })
}

/// A shape's IRI, or a blank node's `_:label`.
fn label(id: &Node) -> String {
    match id {
        Node::Iri(iri) => iri.as_str().to_owned(),
        Node::BlankNode(b) => format!("_:{b}"),
        other => other.to_string(),
    }
}
