//! What a graph validates against: rudof's SHACL and ShEx validators, run on every subject and
//! every shape, the maximal valid shape map of Boneva et al. (ESWC 2017, "Semantics and
//! Validation of Shapes Schemas for RDF").

use std::collections::{BTreeMap, HashSet};
use std::fmt::Debug;

use oxrdf::{NamedNode, Term, Triple};
use prefixmap::{PrefixMap, PrefixMapError};
use rudof_iri::IriS;
use rudof_rdf::rdf_core::query::{QueryRDF, QueryResultFormat, QuerySolutions};
use rudof_rdf::rdf_core::term::Object as Node;
use rudof_rdf::rdf_core::{Matcher, NeighsRDF, Rdf};
use rudof_rdf::rdf_impl::{OxigraphInMemory, OxigraphInMemoryError};
use shacl::ir::{IRSchema, RecursionSemantics};
use shacl::validator::engine::{NativeEngine, Validate};
use shacl::validator::nodes::FocusNodes;
use shacl::validator::report::ValidationResult;
use shex_ast::ir::schema_ir::SchemaIR;
use shex_ast::ir::shape_label::ShapeLabel;
use shex_ast::shapemap::{NodeSelector, QueryShapeMap, ShapeSelector};
use shex_ast::{ObjectValue, ShapeExprLabel};
use shex_validation::{Validator, ValidatorConfig};

use crate::schema::Error;
use crate::shapes::Shapes;

/// The shapes as each validator runs them.
#[derive(Debug)]
pub(crate) enum Document {
    /// Compiled with the greatest fixpoint for recursive references.
    Shacl(Box<IRSchema>),
    ShEx(Box<SchemaIR>),
}

/// For each subject, by IRI, the vertex types (by index) whose shapes it validates against as
/// the document states them, and why it does not validate against each of the others.
pub(crate) struct Validated {
    pub types: BTreeMap<String, Vec<usize>>,
    pub reasons: BTreeMap<String, Vec<String>>,
}

/// Validates every subject of `triples` against every shape of `shapes`.
pub(crate) fn validate(
    shapes: &Shapes,
    subjects: &[&str],
    triples: &[&Triple],
) -> Result<Validated, Error> {
    let mut store = OxigraphInMemory::new();
    for t in triples {
        store
            .add_triple_ref(t.subject.as_ref(), t.predicate.as_ref(), t.object.as_ref())
            .map_err(|e| Error::Validator(e.to_string()))?;
    }
    let mut validated = Validated {
        types: subjects
            .iter()
            .map(|s| ((*s).to_owned(), Vec::new()))
            .collect(),
        reasons: BTreeMap::new(),
    };
    let ids = shapes.ids();
    let failed = match &shapes.document {
        Document::Shacl(ir) => shacl(ir, &store, &ids, subjects)?,
        Document::ShEx(ir) => shex(ir, Data(store), &ids, subjects)?,
    };
    for (v, failures) in failed.into_iter().enumerate() {
        for s in subjects {
            match failures.get(*s) {
                None => validated.types.get_mut(*s).expect("a subject").push(v),
                Some(reason) => validated
                    .reasons
                    .entry((*s).to_owned())
                    .or_default()
                    .push(format!("{}: {reason}", ids[v])),
            }
        }
    }
    Ok(validated)
}

/// Per shape, the subjects that do not validate against it, each with the validator's reason.
type Failures = Vec<BTreeMap<String, String>>;

fn shacl(
    ir: &IRSchema,
    store: &OxigraphInMemory,
    shapes: &[String],
    subjects: &[&str],
) -> Result<Failures, Error> {
    shapes
        .iter()
        .map(|id| {
            let wanted = Node::iri(IriS::new_unchecked(id));
            let (_, shape) = ir
                .iter()
                .find(|(label, _)| **label == wanted)
                .ok_or_else(|| Error::Validator(format!("rudof compiled no shape {id}")))?;
            let mut failures = BTreeMap::new();
            for batch in subjects.chunks(BATCH) {
                let focus = FocusNodes::new(
                    batch
                        .iter()
                        .map(|s| Term::NamedNode(NamedNode::new_unchecked(*s)))
                        .collect(),
                );
                let mut engine = NativeEngine::new(RecursionSemantics::Gfp);
                let outcome = shape
                    .validate(store, &mut engine, Some(&focus), Some(shape), ir)
                    .map_err(|e| Error::Validator(e.to_string()))?;
                for result in outcome.violations() {
                    if let Node::Iri(iri) = result.focus_node() {
                        failures
                            .entry(iri.as_str().to_owned())
                            .or_insert_with(|| said(result));
                    }
                }
            }
            Ok(failures)
        })
        .collect()
}

/// How many focus nodes one SHACL engine validates. The engine memoizes every (node, shape) pair
/// it decides, property shapes included, with the evidence for it, and frees nothing until it is
/// dropped: on LUBM at a million triples one engine for every subject and shape outgrew 15.7 GiB.
/// Whether a node validates against a shape does not depend on the other focus nodes, so an
/// engine per batch gives the same verdicts, re-deciding only the nodes a batch's references
/// reach. No class index is built: it serves targets, and the focus nodes are given.
const BATCH: usize = 4096;

/// A SHACL result as a reader takes it: the path, the validator's message, and the results
/// beneath it (a value that does not conform to a `sh:node` shape, and why).
fn said(result: &ValidationResult) -> String {
    let messages = result.message().messages();
    let message = messages
        .get(&None)
        .or_else(|| messages.values().next())
        .cloned()
        .unwrap_or_else(|| result.constraint_component().to_string());
    let mut text = match result.path() {
        Some(path) => format!("{path}: {message}"),
        None => message,
    };
    for detail in result.details() {
        let focus = match detail.focus_node() {
            Node::Iri(iri) => format!("<{}>", iri.as_str()),
            other => other.to_string(),
        };
        text.push_str(&format!(" ({focus} {})", said(detail)));
    }
    text
}

fn shex(
    ir: &SchemaIR,
    data: Data,
    shapes: &[String],
    subjects: &[&str],
) -> Result<Failures, Error> {
    let fail = |e: &dyn std::fmt::Display| Error::Validator(e.to_string());
    let mut map = QueryShapeMap::new();
    for s in subjects {
        let node = ObjectValue::iri(IriS::new_unchecked(s));
        for id in shapes {
            let label: ShapeExprLabel = (&ShapeLabel::iri(IriS::new_unchecked(id))).into();
            map.add_association(
                NodeSelector::Node(node.clone()),
                &None,
                ShapeSelector::label(label),
                &None,
            )
            .map_err(|e| fail(&e))?;
        }
    }
    let config = ValidatorConfig::default();
    let result = Validator::new(ir, &config)
        .map_err(|e| fail(&e))?
        .validate_shapemap(&map, &data, ir, &None)
        .map_err(|e| fail(&e))?;
    Ok(shapes
        .iter()
        .map(|id| {
            let label = ShapeLabel::iri(IriS::new_unchecked(id));
            subjects
                .iter()
                .filter_map(|s| {
                    let node = shex_ast::Node::new(Node::iri(IriS::new_unchecked(s)));
                    match result.get_info(&node, &label) {
                        Some(status) if status.is_conformant() => None,
                        Some(status) => Some(((*s).to_owned(), status.reason())),
                        None => Some(((*s).to_owned(), "not validated".to_owned())),
                    }
                })
                .collect()
        })
        .collect())
}

/// The graph as rudof's ShEx validator takes it: Oxigraph's in-memory graph, which answers no
/// SPARQL here. The validator asks for SPARQL only to select the nodes of a shape map by a
/// pattern, and the map handed to it names every node.
#[derive(Debug)]
struct Data(OxigraphInMemory);

impl Rdf for Data {
    type IRI = <OxigraphInMemory as Rdf>::IRI;
    type BNode = <OxigraphInMemory as Rdf>::BNode;
    type Literal = <OxigraphInMemory as Rdf>::Literal;
    type Subject = <OxigraphInMemory as Rdf>::Subject;
    type Term = <OxigraphInMemory as Rdf>::Term;
    type Triple = <OxigraphInMemory as Rdf>::Triple;
    type Err = OxigraphInMemoryError;

    fn qualify_iri(&self, iri: &Self::IRI) -> String {
        Rdf::qualify_iri(&self.0, iri)
    }

    fn qualify_subject(&self, subject: &Self::Subject) -> String {
        Rdf::qualify_subject(&self.0, subject)
    }

    fn qualify_term(&self, term: &Self::Term) -> String {
        Rdf::qualify_term(&self.0, term)
    }

    fn prefixmap(&self) -> Option<PrefixMap> {
        Rdf::prefixmap(&self.0)
    }

    fn resolve_prefix_local(&self, prefix: &str, local: &str) -> Result<IriS, PrefixMapError> {
        Rdf::resolve_prefix_local(&self.0, prefix, local)
    }
}

impl NeighsRDF for Data {
    fn triples(&self) -> Result<impl Iterator<Item = Self::Triple>, Self::Err> {
        NeighsRDF::triples(&self.0)
    }

    fn triples_matching<S, P, O>(
        &self,
        subject: &S,
        predicate: &P,
        object: &O,
    ) -> Result<impl Iterator<Item = Self::Triple> + '_, Self::Err>
    where
        S: Matcher<Self::Subject>,
        P: Matcher<Self::IRI>,
        O: Matcher<Self::Term>,
    {
        NeighsRDF::triples_matching(&self.0, subject, predicate, object)
    }
}

impl QueryRDF for Data {
    fn query_select(&self, query: &str) -> Result<QuerySolutions<Self>, Self::Err> {
        Err(no_sparql(query))
    }

    fn query_construct(&self, query: &str, _: &QueryResultFormat) -> Result<String, Self::Err> {
        Err(no_sparql(query))
    }

    fn query_ask(&self, query: &str) -> Result<bool, Self::Err> {
        Err(no_sparql(query))
    }
}

fn no_sparql(query: &str) -> OxigraphInMemoryError {
    OxigraphInMemoryError::RunningQueryError {
        query: query.to_owned(),
        msg: "graphar-rdf validates without SPARQL".to_owned(),
    }
}

/// Whether `subject`'s triples use only predicates its type names: the closed reading of the
/// shape (`sh:closed`, ShEx `CLOSED`), which the layout needs whether or not the shape says it.
pub(crate) fn closed(predicates: &HashSet<&str>, triples: &[&Triple]) -> Option<String> {
    triples
        .iter()
        .find(|t| !predicates.contains(t.predicate.as_str()))
        .map(|t| format!("{} is not a predicate of the shape", t.predicate))
}
