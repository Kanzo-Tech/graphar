//! A [`Plan`] serialized: as RML-Core, `mapping.ttl`, and as R2RML, `mapping.r2rml.ttl`.
//!
//! **RML-Core** (W3C KG-Construct Community Group, Draft Report, 16 March 2026), with RML-IO for
//! the logical sources: a triples map per [`Map`], over its table (`rml:SQL2008Table`) or its query
//! (`rml:SQL2008Query`). A table is `rml:iterator "\"Person\""`, R2RML's delimited table name, so
//! `Person` is not folded to `person` and `Order` is not a reserved word.
//!
//! **R2RML** (W3C Recommendation, 27 September 2012): the same triples maps, over `rr:tableName`
//! or `rr:sqlQuery` with `rr:sqlVersion rr:SQL2008`. Its language is a constant (`rr:language`),
//! so a map with a [`Expr::LangLiteral`] becomes one triples map per language the column holds,
//! each over its query filtered to that language; which languages those are is the data's, so the
//! R2RML document is the writer's to make.
//!
//! **`rml:source` is an anonymous `rml:Source`.** The logical sources are table names a host binds
//! to the chunks when it opens the graph; a source that named a file would claim what the host
//! decides. R2RML has no source at all: its tables are the database's.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use crate::XSD;
use crate::plan::{Expr, Map, Plan, Source, plan, quoted};
use crate::schema::{Error, Schema, plain};

const RML: &str =
    "@prefix rml: <http://w3id.org/rml/> .\n@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .\n";
const R2RML: &str = "@prefix rr: <http://www.w3.org/ns/r2rml#> .\n@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .\n";

/// `mapping.ttl` for `schema`: the RML-Core serialization of its [`plan`].
///
/// # Errors
/// What [`Schema::check`] refuses.
pub fn mapping(schema: &Schema) -> Result<String, Error> {
    Ok(rml(&plan(schema)?))
}

/// The RML-Core serialization of `plan`.
#[must_use]
pub fn rml(plan: &Plan) -> String {
    let mut out = String::from(RML);
    for m in &plan.maps {
        let source = match &m.source {
            Source::Table { .. } => format!(
                "[ rml:source [ a rml:Source ] ; rml:referenceFormulation rml:SQL2008Table ; rml:iterator {} ]",
                literal(&m.source.sql())
            ),
            _ => format!(
                "[ rml:source [ a rml:Source ] ; rml:referenceFormulation rml:SQL2008Query ;\n      rml:iterator {} ]",
                literal(&m.source.sql())
            ),
        };
        let classes: String = m
            .classes
            .iter()
            .map(|c| format!(" ; rml:class {}", iri(c)))
            .collect();
        let subject = format!("[ {}{classes} ]", term_map("rml", &m.subject, None));
        let poms: Vec<String> = m
            .objects
            .iter()
            .map(|(p, o)| {
                format!(
                    "[ rml:predicate {} ;\n      rml:objectMap [ {} ] ]",
                    iri(p),
                    term_map("rml", o, None)
                )
            })
            .collect();
        triples_map(
            &mut out,
            "rml:logicalSource",
            &m.name,
            &source,
            &subject,
            &poms,
        );
    }
    annotations(&mut out, plan);
    out
}

/// The R2RML serialization of `plan`, given the languages each map with a language-tagged object
/// holds, by map name; a map of none holds no triple and has no triples map.
#[must_use]
pub fn r2rml(plan: &Plan, languages: &BTreeMap<String, BTreeSet<String>>) -> String {
    let mut out = String::from(R2RML);
    let none = BTreeSet::new();
    for m in &plan.maps {
        let tagged = m
            .objects
            .iter()
            .any(|(_, o)| matches!(o, Expr::LangLiteral { .. }));
        if !tagged {
            r2rml_map(&mut out, m, &m.name, &table(&m.source), None);
            continue;
        }
        for lang in languages.get(&m.name).unwrap_or(&none) {
            let query = format!(
                "SELECT * FROM ({}) AS q WHERE q.\"lang\" = '{}'",
                m.source.sql(),
                lang.replace('\'', "''")
            );
            let source = format!(
                "[ rr:sqlQuery {} ;\n      rr:sqlVersion rr:SQL2008 ]",
                literal(&query)
            );
            r2rml_map(
                &mut out,
                m,
                &format!("{}.{lang}", m.name),
                &source,
                Some(lang),
            );
        }
    }
    annotations(&mut out, plan);
    out
}

/// A logical table of R2RML.
fn table(source: &Source) -> String {
    match source {
        Source::Table { .. } => format!("[ rr:tableName {} ]", literal(&source.sql())),
        _ => format!(
            "[ rr:sqlQuery {} ;\n      rr:sqlVersion rr:SQL2008 ]",
            literal(&source.sql())
        ),
    }
}

fn r2rml_map(out: &mut String, m: &Map, name: &str, source: &str, lang: Option<&str>) {
    let classes: String = m
        .classes
        .iter()
        .map(|c| format!(" ; rr:class {}", iri(c)))
        .collect();
    let subject = format!("[ {}{classes} ]", term_map("rr", &m.subject, None));
    let poms: Vec<String> = m
        .objects
        .iter()
        .map(|(p, o)| {
            format!(
                "[ rr:predicate {} ;\n      rr:objectMap [ {} ] ]",
                iri(p),
                term_map("rr", o, lang)
            )
        })
        .collect();
    triples_map(out, "rr:logicalTable", name, source, &subject, &poms);
}

/// A term map's body in vocabulary `ns` (`rml` or `rr`): `rml:reference`, `rr:column`.
fn term_map(ns: &str, e: &Expr, lang: Option<&str>) -> String {
    let at = if ns == "rml" { "reference" } else { "column" };
    match e {
        Expr::Iri(a) => format!(
            "{ns}:{at} {} ; {ns}:termType {ns}:IRI",
            literal(&reference(a))
        ),
        Expr::Literal {
            attribute,
            datatype: None,
        } => format!("{ns}:{at} {}", literal(&reference(attribute))),
        Expr::Literal {
            attribute,
            datatype: Some(dt),
        } => format!(
            "{ns}:{at} {} ; {ns}:datatype {}",
            literal(&reference(attribute)),
            iri(dt)
        ),
        Expr::LangLiteral { value, lang: tag } => match lang {
            // R2RML: the language of this triples map, a constant.
            Some(l) => format!(
                "{ns}:{at} {} ; {ns}:language {}",
                literal(&reference(value)),
                literal(l)
            ),
            None => format!(
                "{ns}:{at} {} ; {ns}:languageMap [ {ns}:{at} {} ]",
                literal(&reference(value)),
                literal(&reference(tag))
            ),
        },
    }
}

/// A triples map: `logical` is `rml:logicalSource` or `rr:logicalTable`, and its prefix the
/// vocabulary of the rest.
fn triples_map(
    out: &mut String,
    logical: &str,
    id: &str,
    source: &str,
    subject: &str,
    poms: &[String],
) {
    let ns = logical.split(':').next().expect("a prefixed name");
    write!(
        out,
        "\n<#{}> a {ns}:TriplesMap ;\n  {logical} {source} ;\n  {ns}:subjectMap {subject}",
        fragment(id)
    )
    .expect("a String");
    for pom in poms {
        write!(out, " ;\n  {ns}:predicateObjectMap {pom}").expect("a String");
    }
    out.push_str(" .\n");
}

fn annotations(out: &mut String, plan: &Plan) {
    if !plan.annotations.is_empty() {
        out.push('\n');
        for t in &plan.annotations {
            writeln!(out, "{t} .").expect("a String");
        }
    }
}

/// An IRI, `xsd:` where it is one.
fn iri(text: &str) -> String {
    match text.strip_prefix(XSD) {
        Some(local) if plain(local) => format!("xsd:{local}"),
        _ => format!("<{text}>"),
    }
}

/// A Turtle string.
fn literal(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A triples map's name, a fragment of the document: the name with every character outside
/// unreserved ones percent-encoded.
fn fragment(name: &str) -> String {
    let mut out = String::new();
    for b in name.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(char::from(b));
        } else {
            write!(out, "%{b:02X}").expect("a String");
        }
    }
    out
}

/// An attribute as a reference: its name, delimited where it is not a plain identifier (RML-IO
/// and R2RML: a reference to a relational source is a column name).
fn reference(name: &str) -> String {
    if plain(name) {
        name.to_owned()
    } else {
        quoted(name)
    }
}
