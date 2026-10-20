//! SHACL Core shapes graphs in Turtle (W3C Recommendation, 20 July 2017), read by rudof: its
//! `rudof_rdf` parses the Turtle and `shacl`'s `ShaclParser` turns the graph into shapes. This
//! module only reads the fragment the module comment of [`crate::shapes`] names off them.

use rudof_rdf::rdf_core::term::Object as Node;
use rudof_rdf::rdf_core::{RDFFormat, SHACLPath};
use rudof_rdf::rdf_impl::{OxigraphInMemory, ReaderMode};
use shacl::ast::{ASTComponent, ASTSchema, ASTShape};
use shacl::rdf::ShaclParser;
use shacl::types::{NodeKind, Target as ShaclTarget};

use super::{Shape, Target};
use crate::RDF_LANG_STRING;
use crate::schema::{Cardinality, Error, Object, Schema};

/// The schema of a SHACL shapes graph written in Turtle.
///
/// # Errors
/// [`Error::Shapes`] when the document does not parse or rudof refuses the shapes in it.
pub fn from_turtle(text: &str) -> Result<Schema, Error> {
    let graph = OxigraphInMemory::from_str(text, &RDFFormat::Turtle, None, &ReaderMode::Strict)
        .map_err(|e| Error::Shapes(e.to_string()))?;
    let schema = ShaclParser::new(graph)
        .parse()
        .map_err(|e| Error::Shapes(e.to_string()))?;
    Ok(super::schema(&shapes(&schema)))
}

fn shapes(schema: &ASTSchema) -> Vec<Shape> {
    schema
        .iter()
        .filter_map(|(id, shape)| match shape {
            ASTShape::NodeShape(node) => Some((id, node)),
            ASTShape::PropertyShape(_) => None,
        })
        .map(|(id, node)| {
            // `sh:targetClass`, or the shape itself when it is a class (SHACL 2.1.3.1).
            let class = node.targets().iter().find_map(|t| match t {
                ShaclTarget::Class(Node::Iri(c)) | ShaclTarget::ImplicitClass(Node::Iri(c)) => {
                    Some(c.as_str().to_owned())
                }
                _ => None,
            });
            let properties = node
                .property_shapes()
                .iter()
                .filter_map(|p| match schema.get_shape(p) {
                    Some(ASTShape::PropertyShape(p)) => Some(p),
                    _ => None,
                })
                .filter_map(|p| {
                    let SHACLPath::Predicate { pred } = p.path() else {
                        return None;
                    };
                    Some((pred.as_str().to_owned(), target(p.components())?, {
                        let single = p
                            .components()
                            .iter()
                            .any(|c| matches!(c, ASTComponent::MaxCount(1)));
                        if single {
                            Cardinality::Single
                        } else {
                            Cardinality::Set
                        }
                    }))
                })
                .collect();
            Shape {
                id: label(id),
                class,
                properties,
            }
        })
        .collect()
}

/// What a property shape's values are, by the first of `sh:datatype`, `sh:languageIn`,
/// `sh:class`, `sh:node` and `sh:nodeKind sh:IRI` it has.
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
            ASTComponent::Class(Node::Iri(c)) => Some(Target::Class(c.as_str().to_owned())),
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
            ASTComponent::NodeKind(NodeKind::Iri) => Some(Target::Column(Object::Iri)),
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
