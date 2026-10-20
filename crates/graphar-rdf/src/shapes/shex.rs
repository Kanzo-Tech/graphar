//! ShEx 2.1 schemas in ShExJ, the JSON-LD form every ShEx implementation reads and writes, in the
//! fragment the module comment of [`crate::shapes`] names. ShExC converts to it with any of them
//! (`shex.js`, rudof, PyShEx); this crate parses no compact syntax.

use serde_json::Value;

use super::{Shape, Target};
use crate::schema::{Cardinality, Error, Object, Schema};
use crate::{RDF_LANG_STRING, RDF_TYPE};

/// The schema of a ShEx schema in ShExJ.
///
/// # Errors
/// [`Error::Shapes`] when the document is not JSON or not a ShEx schema.
pub fn from_json(text: &str) -> Result<Schema, Error> {
    let doc: Value = serde_json::from_str(text).map_err(|e| Error::Shapes(e.to_string()))?;
    if doc.get("type").and_then(Value::as_str) != Some("Schema") {
        return Err(Error::Shapes("not a ShExJ `Schema`".to_owned()));
    }
    let shapes = doc
        .get("shapes")
        .and_then(Value::as_array)
        .map(|shapes| shapes.iter().filter_map(shape).collect::<Vec<_>>())
        .unwrap_or_default();
    Ok(super::schema(&shapes))
}

/// A `Shape`, or a `ShapeDecl` (ShEx 2.1) around one.
fn shape(decl: &Value) -> Option<Shape> {
    let id = decl.get("id")?.as_str()?.to_owned();
    let expr = match decl.get("type")?.as_str()? {
        "ShapeDecl" => decl.get("shapeExpr")?,
        "Shape" => decl,
        _ => return None,
    };
    if expr.get("type")?.as_str()? != "Shape" {
        return None;
    }
    let mut constraints = Vec::new();
    if let Some(e) = expr.get("expression") {
        flatten(e, &mut constraints);
    }
    let mut class = None;
    let mut properties = Vec::new();
    for tc in constraints {
        if tc.get("inverse").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        let Some(predicate) = tc.get("predicate").and_then(Value::as_str) else {
            continue;
        };
        let value = tc.get("valueExpr");
        if predicate == RDF_TYPE {
            let values = value
                .and_then(|v| v.get("values"))
                .and_then(Value::as_array);
            if let Some([only]) = values.map(Vec::as_slice)
                && let Some(c) = only.as_str()
            {
                class.get_or_insert_with(|| c.to_owned());
            }
            continue;
        }
        let target = match value {
            Some(Value::String(reference)) => Target::Shape(reference.clone()),
            Some(v) if v.get("type").and_then(Value::as_str) == Some("NodeConstraint") => {
                if let Some(dt) = v.get("datatype").and_then(Value::as_str) {
                    Target::Column(if dt == RDF_LANG_STRING {
                        Object::LangString
                    } else {
                        Object::Literal(dt.to_owned())
                    })
                } else if v.get("nodeKind").and_then(Value::as_str) == Some("iri") {
                    Target::Column(Object::Iri)
                } else {
                    continue;
                }
            }
            _ => continue,
        };
        // `max` defaults to 1, and -1 is unbounded.
        let max = tc.get("max").and_then(Value::as_i64).unwrap_or(1);
        let cardinality = if max == 1 {
            Cardinality::Single
        } else {
            Cardinality::Set
        };
        properties.push((predicate.to_owned(), target, cardinality));
    }
    Some(Shape {
        id,
        class,
        properties,
    })
}

/// The triple constraints of an `EachOf` (or of one constraint), in order. A `OneOf` is a choice
/// and types nothing.
fn flatten<'a>(e: &'a Value, out: &mut Vec<&'a Value>) {
    match e.get("type").and_then(Value::as_str) {
        Some("TripleConstraint") => out.push(e),
        Some("EachOf") => {
            for x in e
                .get("expressions")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                flatten(x, out);
            }
        }
        _ => {}
    }
}
