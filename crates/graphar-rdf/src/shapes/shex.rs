//! ShEx 2.1 schemas, in ShExC or ShExJ, read by rudof's `shex_ast`. This module only reads the
//! fragment the module comment of [`crate::shapes`] names off the schema it parses.

use prefixmap::{IriRef, PrefixMap};
use rudof_iri::IriS;
use shex_ast::ir::ast2ir::AST2IR;
use shex_ast::ir::map_state::MapState;
use shex_ast::ir::schema_ir::SchemaIR;
use shex_ast::ir::semantic_actions_registry::SemanticActionsRegistry;
use shex_ast::{
    NodeKind, ObjectValue, ResolveMethod, Schema as ShEx, ShExParser, ShapeExpr, ShapeExprLabel,
    TripleExpr, ValueSetValue,
};
use shex_validation::ValidatorConfig;

use super::{Shape, Shapes, Syntax, Target};
use crate::schema::{Cardinality, Error, Object};
use crate::validate::Document;
use crate::{RDF_LANG_STRING, RDF_TYPE};

/// The source IRI that names a document in rudof's errors; nothing resolves against it.
const SOURCE: &str = "urn:graphar-rdf:shex";

/// The shapes of a ShEx schema in ShExJ, the JSON-LD form.
///
/// # Errors
/// [`Error::Shapes`] when the document is not a ShExJ schema.
pub fn from_json(text: &str) -> Result<Shapes, Error> {
    let shex = ShEx::from_reader(text.as_bytes()).map_err(|e| Error::Shapes(e.to_string()))?;
    compiled(&shex, Syntax::ShExJ, text)
}

/// The shapes of a ShEx schema in ShExC, the compact syntax.
///
/// # Errors
/// [`Error::Shapes`] when the document does not parse.
pub fn from_compact(text: &str) -> Result<Shapes, Error> {
    let source = IriS::new_unchecked(SOURCE);
    let shex = ShExParser::parse(text, None, &source).map_err(|e| Error::Shapes(e.to_string()))?;
    compiled(&shex, Syntax::ShExC, text)
}

/// The shapes of `shex`, and the schema rudof's validator runs on, compiled as rudof compiles it.
fn compiled(shex: &ShEx, syntax: Syntax, text: &str) -> Result<Shapes, Error> {
    let source = IriS::new_unchecked(SOURCE);
    let mut map_state = MapState::default();
    let registry = SemanticActionsRegistry::default();
    registry.set_map_state(&mut map_state);
    let mut ir = SchemaIR::new(registry);
    AST2IR::new(&ResolveMethod::default(), map_state)
        .compile(
            shex,
            &source,
            &None,
            &mut ir,
            ValidatorConfig::default().external_resolvers(),
        )
        .map_err(|e| Error::Shapes(e.to_string()))?;
    Ok(super::read(
        &shapes(shex)?,
        Document::ShEx(Box::new(ir)),
        syntax,
        text,
    ))
}

fn shapes(shex: &ShEx) -> Result<Vec<Shape>, Error> {
    let prefixes = shex.prefixmap().unwrap_or_default();
    let iri = |r: &IriRef| resolve(r, &prefixes);
    shex.shapes()
        .unwrap_or_default()
        .iter()
        .filter_map(|decl| {
            let id = label(&decl.id, &prefixes)?;
            let ShapeExpr::Shape(shape) = &decl.shape_expr else {
                return None;
            };
            Some((id, shape))
        })
        .map(|(id, shape)| {
            let mut constraints = Vec::new();
            if let Some(e) = &shape.expression {
                flatten(&e.te, &mut constraints);
            }
            let mut class = None;
            let mut properties = Vec::new();
            for tc in constraints {
                let TripleExpr::TripleConstraint {
                    inverse,
                    predicate,
                    value_expr,
                    min,
                    max,
                    ..
                } = tc
                else {
                    continue;
                };
                if *inverse == Some(true) {
                    continue;
                }
                let Some(predicate) = iri(predicate) else {
                    continue;
                };
                if *max == Some(0) {
                    return Err(super::forbidden(&id, &predicate));
                }
                let value = value_expr.as_deref();
                let values = |value: Option<&ShapeExpr>| match value {
                    Some(ShapeExpr::NodeConstraint(nc)) => nc.values(),
                    _ => None,
                };
                // `min` and `max` default to 1: `rdf:type [c]` is exactly one class triple.
                if predicate == RDF_TYPE
                    && min.unwrap_or(1) == 1
                    && max.unwrap_or(1) == 1
                    && let Some([ValueSetValue::ObjectValue(ObjectValue::IriRef(c))]) =
                        values(value).as_deref()
                    && let Some(c) = iri(c)
                {
                    class.get_or_insert(c);
                    continue;
                }
                let target = match value {
                    Some(ShapeExpr::Ref(reference)) => match label(reference, &prefixes) {
                        Some(reference) => Target::Shape(reference),
                        None => continue,
                    },
                    Some(ShapeExpr::NodeConstraint(nc)) => {
                        let iris = nc.values().is_some_and(|vs| {
                            !vs.is_empty()
                                && vs.iter().all(|v| {
                                    matches!(v, ValueSetValue::ObjectValue(ObjectValue::IriRef(_)))
                                })
                        });
                        match (nc.datatype().as_ref().and_then(iri), nc.node_kind()) {
                            (Some(dt), _) if dt == RDF_LANG_STRING => {
                                Target::Column(Object::LangString)
                            }
                            (Some(dt), _) => Target::Column(Object::Literal(dt)),
                            (None, Some(NodeKind::Iri)) => Target::Column(Object::Iri),
                            (None, None) if iris => Target::Column(Object::Iri),
                            _ => continue,
                        }
                    }
                    _ => continue,
                };
                // `max` defaults to 1, and -1 is unbounded.
                let cardinality = if max.unwrap_or(1) == 1 {
                    Cardinality::Single
                } else {
                    Cardinality::Set
                };
                properties.push((predicate, target, cardinality));
            }
            Ok(Shape {
                id,
                class,
                properties,
            })
        })
        .collect()
}

/// The triple constraints of an `EachOf` (or of one constraint), in order. A `OneOf` is a choice
/// and types nothing.
fn flatten<'a>(e: &'a TripleExpr, out: &mut Vec<&'a TripleExpr>) {
    match e {
        TripleExpr::TripleConstraint { .. } => out.push(e),
        TripleExpr::EachOf { expressions, .. } => {
            for x in expressions {
                flatten(&x.te, out);
            }
        }
        TripleExpr::OneOf { .. } | TripleExpr::Ref(_) => {}
    }
}

fn resolve(r: &IriRef, prefixes: &PrefixMap) -> Option<String> {
    match r {
        IriRef::Iri(iri) => Some(iri.as_str().to_owned()),
        IriRef::Prefixed { prefix, local } => prefixes
            .resolve_prefix_local(prefix.as_str(), local.as_str())
            .ok()
            .map(|iri| iri.as_str().to_owned()),
    }
}

/// A shape label as an IRI or `_:label`; `START` labels nothing.
fn label(l: &ShapeExprLabel, prefixes: &PrefixMap) -> Option<String> {
    match l {
        ShapeExprLabel::IriRef { value } => resolve(value, prefixes),
        ShapeExprLabel::BNode { value } => Some(format!("_:{value}")),
        ShapeExprLabel::Start => None,
    }
}
