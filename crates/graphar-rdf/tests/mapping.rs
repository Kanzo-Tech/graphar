//! `mapping.ttl`: Turtle that parses, and SQL a case-sensitive engine reads as written (F1 to F3
//! of the run over fossil's programs: undelimited table and index-column names, `n.value`, and a
//! derived table with no correlation name, all accepted by DuckDB and refused by PostgreSQL).
//! That PostgreSQL does accept it is `conformance/rml/run.py`'s, in CI.

use graphar_rdf::{Cardinality, Column, EdgeType, Object, Schema, VertexType, mapping};
use oxrdf::Term;
use oxttl::TurtleParser;

fn column(name: &str, object: Object, cardinality: Cardinality) -> Column {
    Column {
        name: name.to_owned(),
        predicate: format!("https://example.org/{name}"),
        object,
        cardinality,
    }
}

/// A type named by an SQL reserved word, in mixed case, with a set and a tagged value.
fn schema() -> Schema {
    Schema {
        vertices: vec![
            VertexType {
                label: "Order".to_owned(),
                class: Some("https://example.org/Order".to_owned()),
                columns: vec![
                    column(
                        "placedOn",
                        Object::Literal("http://www.w3.org/2001/XMLSchema#date".into()),
                        Cardinality::Single,
                    ),
                    column("labels", Object::LangString, Cardinality::Set),
                    column("note", Object::LangString, Cardinality::Single),
                    column(
                        "birth-year",
                        Object::Literal("http://www.w3.org/2001/XMLSchema#gYear".into()),
                        Cardinality::Single,
                    ),
                ],
            },
            VertexType {
                label: "Customer".to_owned(),
                class: Some("https://example.org/Customer".to_owned()),
                columns: vec![],
            },
        ],
        edges: vec![EdgeType {
            label: "placedBy".to_owned(),
            source: "Order".to_owned(),
            destination: "Customer".to_owned(),
            predicate: "https://example.org/placedBy".to_owned(),
        }],
        annotations: vec![],
    }
}

fn iterators(text: &str) -> Vec<String> {
    TurtleParser::new()
        .with_base_iri("http://example.org/mapping.ttl")
        .expect("a base")
        .for_slice(text.as_bytes())
        .map(|t| t.expect("Turtle"))
        .filter(|t| t.predicate.as_str() == "http://w3id.org/rml/iterator")
        .map(|t| match t.object {
            Term::Literal(l) => l.value().to_owned(),
            other => panic!("{other}"),
        })
        .collect()
}

/// `sql` with every `"…"` and `'…'` replaced by a space.
fn undelimited(sql: &str) -> String {
    let mut out = String::new();
    let mut quote = None;
    for c in sql.chars() {
        match (quote, c) {
            (None, '"' | '\'') => quote = Some(c),
            (None, c) => out.push(c),
            (Some(q), c) if c == q => {
                quote = None;
                out.push(' ');
            }
            (Some(_), _) => {}
        }
    }
    out
}

#[test]
fn every_table_is_a_delimited_name_and_every_query_standard_sql() {
    let text = mapping(&schema()).expect("a mapping");
    let iterators = iterators(&text);
    // Two tables, two nested columns (the set and the tagged value), one edge.
    assert_eq!(iterators.len(), 2 + 2 + 1, "{text}");
    for it in &iterators {
        if !it.starts_with("SELECT") {
            assert!(
                it.starts_with('"') && it.ends_with('"'),
                "a table name delimited: {it}"
            );
            continue;
        }
        // Every name a query reads is delimited: with the delimited names and the string
        // constants taken out, no index column, table, column or struct field is left.
        let bare = undelimited(it);
        for name in [
            "_graphArSrcIndex",
            "_graphArDstIndex",
            "_graphArVertexIndex",
            "Order",
            "Customer",
            "value",
            "lang",
            "subject",
            "labels",
            "note",
        ] {
            assert!(
                !bare
                    .split(|c: char| !c.is_alphanumeric() && c != '_')
                    .any(|w| w == name),
                "`{name}` is not delimited in {it}"
            );
        }
        assert!(
            !it.contains("n.value") && !it.contains("n.lang"),
            "a struct field is (n).\"value\": {it}"
        );
        if it.contains("FROM (SELECT") {
            assert!(
                it.trim_end().ends_with(") AS t"),
                "a derived table has a correlation name: {it}"
            );
        }
    }
    // A reference to a column whose name is not a plain identifier is delimited too.
    assert!(text.contains(r#"rml:reference "\"birth-year\"""#), "{text}");
}

#[test]
fn a2_refuses_one_iri_for_two_things() {
    let mut s = schema();
    s.vertices[1].class = s.vertices[0].class.clone();
    let err = mapping(&s).expect_err("two types, one class");
    assert!(
        err.to_string()
            .contains("names both `Order` and `Customer`"),
        "{err}"
    );

    let mut s = schema();
    s.edges[0].predicate = s.vertices[0].columns[0].predicate.clone();
    assert!(
        mapping(&s).is_err(),
        "a relation is not a column of its source"
    );
}
