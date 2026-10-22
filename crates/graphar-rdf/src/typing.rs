//! Which vertex type each subject belongs to: the shape it validates against.

use std::collections::{BTreeMap, HashMap, HashSet};

use oxrdf::{Term, Triple};

use crate::RDF_TYPE;
use crate::schema::{Error, Schema};
use crate::shapes::Shapes;
use crate::validate::{closed, validate};

/// How [`crate::write()`] knows each subject's type. A type is a shape, and a subject belongs to
/// the shape it validates against, whatever its triples claim: `rdf:type` is a predicate like any
/// other. Either way the writer then places every triple, which validates the structure of each
/// shape (its columns, their value types and multiplicity, its references), and refuses the graph
/// if a triple has no place.
#[derive(Debug)]
pub enum Typing<'a> {
    /// Every subject validated against every shape by rudof's validator, SHACL's or ShEx's as the
    /// document is, each shape read closed: the maximal valid shape map. A subject that validates
    /// against no shape, or against more than one, is refused with the validator's reasons.
    ///
    /// The validator takes the whole graph in memory (until rudof reads it through SQL), so under
    /// this typing [`crate::write()`] holds every triple, types every subject, and only then
    /// places and writes them: it does not stream.
    Validate(&'a Shapes),
    /// The caller's shape map over `schema`, which the caller guarantees valid: nothing is
    /// validated but the structure placement checks. A subject is typed from its own triples as
    /// it arrives, so under this typing [`crate::write()`] streams.
    Assume(&'a Schema, &'a ShapeMap),
}

/// A shape map a caller states: which type each subject has.
#[derive(Clone, Debug)]
pub enum ShapeMap {
    /// `{FOCUS rdf:type c}@s` for every type `s` whose shape fixes the class `c`: a subject has
    /// the type of the class of its `rdf:type` triple.
    ByClass,
    /// The type of each subject, by its IRI and the type's label.
    Nodes(HashMap<String, String>),
}

impl Typing<'_> {
    /// The schema the subjects are typed in.
    #[must_use]
    pub fn schema(&self) -> &Schema {
        match self {
            Self::Validate(shapes) => shapes.schema(),
            Self::Assume(schema, _) => schema,
        }
    }
}

/// `types`, if it is exactly one, else the refusal naming `subject`, the types and the reasons.
fn one(
    schema: &Schema,
    subject: &str,
    types: Vec<usize>,
    reasons: Vec<String>,
) -> Result<usize, Error> {
    match types.as_slice() {
        [v] => Ok(*v),
        _ => Err(Error::Typing {
            subject: subject.to_owned(),
            types: types
                .into_iter()
                .map(|v| schema.vertices[v].label.clone())
                .collect(),
            reasons,
        }),
    }
}

/// Each subject's type, by index in the schema, from rudof's validator: every subject of the
/// graph at once, which is why [`Typing::Validate`] holds the graph in memory.
pub(crate) fn validated(
    shapes: &Shapes,
    by_subject: &BTreeMap<String, Vec<Triple>>,
) -> Result<HashMap<String, usize>, Error> {
    let schema = shapes.schema();
    let subjects: Vec<&str> = by_subject.keys().map(String::as_str).collect();
    let triples: Vec<&Triple> = by_subject.values().flatten().collect();
    let validated = validate(shapes, &subjects, &triples)?;
    let ids = shapes.ids();
    let predicates: Vec<HashSet<&str>> = schema
        .vertices
        .iter()
        .map(|v| {
            let mut p: HashSet<&str> = v.columns.iter().map(|c| c.predicate.as_str()).collect();
            p.extend(
                schema
                    .edges
                    .iter()
                    .filter(|e| e.source == v.label)
                    .map(|e| e.predicate.as_str()),
            );
            if v.class.is_some() {
                p.insert(RDF_TYPE);
            }
            p
        })
        .collect();
    let mut typed = HashMap::new();
    for (subject, ts) in by_subject {
        let ts: Vec<&Triple> = ts.iter().collect();
        let mut reasons = validated.reasons.get(subject).cloned().unwrap_or_default();
        let types = validated.types[subject]
            .iter()
            .copied()
            .filter(|&v| match closed(&predicates[v], &ts) {
                None => true,
                Some(reason) => {
                    reasons.push(format!("{}: {reason}", ids[v]));
                    false
                }
            })
            .collect();
        typed.insert(subject.clone(), one(schema, subject, types, reasons)?);
    }
    Ok(typed)
}

/// One subject's type, by index in `schema`, from the caller's shape map: the subject and its
/// triples alone, so [`Typing::Assume`] types a subject as it arrives.
pub(crate) fn assumed(
    schema: &Schema,
    map: &ShapeMap,
    subject: &str,
    triples: &[Triple],
) -> Result<usize, Error> {
    let types = match map {
        ShapeMap::ByClass => {
            let classes: HashSet<&str> = triples
                .iter()
                .filter(|t| t.predicate.as_str() == RDF_TYPE)
                .filter_map(|t| match &t.object {
                    Term::NamedNode(c) => Some(c.as_str()),
                    _ => None,
                })
                .collect();
            (0..schema.vertices.len())
                .filter(|&v| {
                    schema.vertices[v]
                        .class
                        .as_deref()
                        .is_some_and(|c| classes.contains(c))
                })
                .collect()
        }
        ShapeMap::Nodes(map) => map
            .get(subject)
            .and_then(|l| schema.vertices.iter().position(|v| &v.label == l))
            .into_iter()
            .collect(),
    };
    one(schema, subject, types, Vec::new())
}
