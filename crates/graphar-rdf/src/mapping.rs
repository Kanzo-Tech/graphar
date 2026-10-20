//! The RML-Core mapping of a [`Schema`]: what the tables mean as RDF, as `mapping.ttl`.
//!
//! **The reference is RML-Core** (W3C KG-Construct Community Group, Draft Report, 16 March 2026)
//! with RML-IO for the logical sources. One triples map per vertex type, over its table
//! (`rml:SQL2008Table`); one per edge type, over a query that joins the adjacency list to the two
//! end tables on the internal ids and reads the two subjects; one per set or language-tagged
//! column, over a query that unnests it so each row is one value; and eight over the residual
//! table, one per kind of subject (IRI, blank node) and object (IRI, blank node, literal with a
//! datatype, literal with a language tag), each filtered to its rows.
//!
//! **Every identifier is delimited, and every query is SQL:2008.** A table is `rml:iterator
//! "\"Person\""`, R2RML's delimited table name, so `Person` is not folded to `person` and `Order`
//! is not a reserved word; every column in a query is quoted; a struct field is `(n)."value"`, and
//! a derived table has a correlation name. Three processors on two engines disagreed with an
//! earlier generator on exactly these (`rml-motores`, findings F1 to F3), and `DuckDB`, whose
//! identifiers fold case, accepted all three. `unnest` stays: it is SQL's feature S091.
//!
//! **No term map reads an internal id, a row position or a generated blank node.** The ids appear
//! only in the edge queries' joins, so the triples do not depend on the layout. A blank node of
//! the residual table is the label it was given, read by reference.
//!
//! **`rml:source` is an anonymous `rml:Source`.** The logical sources are table names a host binds
//! to the chunks when it opens the graph; a source that named a file would claim what the host
//! decides.

use std::fmt::Write as _;

use crate::schema::{Cardinality, Error, Object, RESIDUAL, Schema, Storage, plain};
use crate::{RDF_LANG_STRING, SUBJECT, XSD, XSD_STRING};

const PREFIXES: &str =
    "@prefix rml: <http://w3id.org/rml/> .\n@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .\n";

/// `mapping.ttl` for `schema`.
///
/// # Errors
/// What [`Schema::check`] refuses.
pub fn mapping(schema: &Schema) -> Result<String, Error> {
    schema.check()?;
    let mut out = String::from(PREFIXES);
    for v in &schema.vertices {
        let table = quoted(&v.label);
        let scalars: Vec<String> = v
            .columns
            .iter()
            .filter(|c| c.cardinality == Cardinality::Single && c.storage() != Storage::LangString)
            .map(|c| predicate_object(&c.predicate, &object(&c.object, &reference(&c.name))))
            .collect();
        if v.class.is_some() || !scalars.is_empty() {
            let classes = v
                .class
                .iter()
                .map(|c| format!(" ; rml:class {}", iri(c)))
                .collect::<String>();
            triples_map(
                &mut out,
                &v.label,
                &table_source(&table),
                &format!("[ rml:reference \"{SUBJECT}\" ; rml:termType rml:IRI{classes} ]"),
                &scalars,
            );
        }
        for c in &v.columns {
            let tagged = c.storage() == Storage::LangString;
            let set = c.cardinality == Cardinality::Set;
            if !tagged && !set {
                continue;
            }
            let column = quoted(&c.name);
            let query = match (set, tagged) {
                (true, true) => format!(
                    "SELECT t.\"{SUBJECT}\", (t.n).\"value\" AS \"value\", (t.n).\"lang\" AS \"lang\" \
                     FROM (SELECT \"{SUBJECT}\", unnest({column}) AS n FROM {table}) AS t"
                ),
                (true, false) => {
                    format!("SELECT \"{SUBJECT}\", unnest({column}) AS \"value\" FROM {table}")
                }
                _ => format!(
                    "SELECT \"{SUBJECT}\", ({column}).\"value\" AS \"value\", ({column}).\"lang\" AS \"lang\" \
                     FROM {table}"
                ),
            };
            triples_map(
                &mut out,
                &format!("{}_{}", v.label, c.name),
                &query_source(&query),
                &format!("[ rml:reference \"{SUBJECT}\" ; rml:termType rml:IRI ]"),
                &[predicate_object(&c.predicate, &object(&c.object, "value"))],
            );
        }
    }
    for e in &schema.edges {
        let query = format!(
            "SELECT s.\"{SUBJECT}\" AS \"source\", d.\"{SUBJECT}\" AS \"target\" FROM {edge} AS e \
             JOIN {src} AS s ON e.\"{SRC}\" = s.\"{VID}\" \
             JOIN {dst} AS d ON e.\"{DST}\" = d.\"{VID}\"",
            edge = quoted(&e.triple()),
            src = quoted(&e.source),
            dst = quoted(&e.destination),
            SRC = graphar_parquet::SRC_INDEX_COL,
            DST = graphar_parquet::DST_INDEX_COL,
            VID = graphar_parquet::VERTEX_INDEX_COL,
        );
        triples_map(
            &mut out,
            &e.triple(),
            &query_source(&query),
            "[ rml:reference \"source\" ; rml:termType rml:IRI ]",
            &[predicate_object(
                &e.predicate,
                "[ rml:reference \"target\" ; rml:termType rml:IRI ]",
            )],
        );
    }
    residual(&mut out);
    if !schema.annotations.is_empty() {
        out.push('\n');
        for t in &schema.annotations {
            writeln!(out, "{t} .").expect("a String");
        }
    }
    Ok(out)
}

/// The eight triples maps of the residual table.
fn residual(out: &mut String) {
    let table = quoted(RESIDUAL);
    let blank = |column: &str| format!("SUBSTRING(\"{column}\" FROM 1 FOR 2) = '_:'");
    let label = |column: &str| format!("SUBSTRING(\"{column}\" FROM 3) AS \"{column}\"");
    let subjects = [
        (
            "iri",
            format!("\"{SUBJECT}\""),
            format!("NOT {}", blank(SUBJECT)),
            "rml:IRI",
        ),
        ("blank", label(SUBJECT), blank(SUBJECT), "rml:BlankNode"),
    ];
    let objects = [
        (
            "iri",
            "\"object\"".to_owned(),
            format!("\"datatype\" IS NULL AND NOT {}", blank("object")),
            "[ rml:reference \"object\" ; rml:termType rml:IRI ]".to_owned(),
        ),
        (
            "blank",
            label("object"),
            format!("\"datatype\" IS NULL AND {}", blank("object")),
            "[ rml:reference \"object\" ; rml:termType rml:BlankNode ]".to_owned(),
        ),
        (
            "typed",
            "\"object\", \"datatype\"".to_owned(),
            "\"datatype\" IS NOT NULL AND \"lang\" IS NULL".to_owned(),
            "[ rml:reference \"object\" ; rml:termType rml:Literal ; \
             rml:datatypeMap [ rml:reference \"datatype\" ] ]"
                .to_owned(),
        ),
        (
            "tagged",
            "\"object\", \"lang\"".to_owned(),
            "\"lang\" IS NOT NULL".to_owned(),
            "[ rml:reference \"object\" ; rml:termType rml:Literal ; \
             rml:languageMap [ rml:reference \"lang\" ] ]"
                .to_owned(),
        ),
    ];
    for (s_kind, s_select, s_filter, s_term) in &subjects {
        for (o_kind, o_select, o_filter, o_map) in &objects {
            let query = format!(
                "SELECT {s_select}, \"predicate\", {o_select} FROM {table} \
                 WHERE {s_filter} AND {o_filter}"
            );
            triples_map(
                out,
                &format!("{RESIDUAL}_{s_kind}_{o_kind}"),
                &query_source(&query),
                &format!("[ rml:reference \"{SUBJECT}\" ; rml:termType {s_term} ]"),
                &[format!(
                    "[ rml:predicateMap [ rml:reference \"predicate\" ; rml:termType rml:IRI ] ;\n      rml:objectMap {o_map} ]"
                )],
            );
        }
    }
}

fn triples_map(out: &mut String, id: &str, source: &str, subject: &str, poms: &[String]) {
    write!(
        out,
        "\n<#{}> a rml:TriplesMap ;\n  rml:logicalSource {source} ;\n  rml:subjectMap {subject}",
        fragment(id)
    )
    .expect("a String");
    for pom in poms {
        write!(out, " ;\n  rml:predicateObjectMap {pom}").expect("a String");
    }
    out.push_str(" .\n");
}

fn table_source(table: &str) -> String {
    format!(
        "[ rml:source [ a rml:Source ] ; rml:referenceFormulation rml:SQL2008Table ; rml:iterator {} ]",
        literal(table)
    )
}

fn query_source(query: &str) -> String {
    format!(
        "[ rml:source [ a rml:Source ] ; rml:referenceFormulation rml:SQL2008Query ;\n      rml:iterator {} ]",
        literal(query)
    )
}

fn predicate_object(predicate: &str, object: &str) -> String {
    format!(
        "[ rml:predicate {} ;\n      rml:objectMap {object} ]",
        iri(predicate)
    )
}

/// The object map of a column's values, read by `reference`.
fn object(kind: &Object, reference: &str) -> String {
    let reference = literal(reference);
    match kind {
        Object::Iri => format!("[ rml:reference {reference} ; rml:termType rml:IRI ]"),
        Object::LangString => lang(&reference),
        Object::Literal(dt) if dt == RDF_LANG_STRING => lang(&reference),
        // A simple literal is `xsd:string` (RDF 1.1), and a column's natural datatype for text.
        Object::Literal(dt) if dt == XSD_STRING => format!("[ rml:reference {reference} ]"),
        Object::Literal(dt) => format!("[ rml:reference {reference} ; rml:datatype {} ]", iri(dt)),
    }
}

fn lang(reference: &str) -> String {
    format!("[ rml:reference {reference} ; rml:languageMap [ rml:reference \"lang\" ] ]")
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

/// A column as a reference: its name, delimited where it is not a plain identifier (RML-IO: a
/// reference to a relational source is a column name).
fn reference(name: &str) -> String {
    if plain(name) {
        name.to_owned()
    } else {
        quoted(name)
    }
}

/// A name as a SQL delimited identifier.
pub(crate) fn quoted(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}
