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
                shape: None,
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
                shape: None,
                columns: vec![],
            },
        ],
        edges: vec![EdgeType {
            label: "placedBy".to_owned(),
            source: "Order".to_owned(),
            destination: "Customer".to_owned(),
            predicate: "https://example.org/placedBy".to_owned(),
        }],
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
    // Two types may fix one class: a type is a shape, and the class is a triple the map writes.
    let mut s = schema();
    s.vertices[1].class = s.vertices[0].class.clone();
    assert!(mapping(&s).is_ok());

    let mut s = schema();
    s.edges[0].predicate = s.vertices[0].columns[0].predicate.clone();
    assert!(
        mapping(&s).is_err(),
        "a relation is not a column of its source"
    );
}

#[test]
fn rml_and_r2rml_are_one_plan_and_r2rml_has_a_map_per_language() {
    let plan = graphar_rdf::plan(&schema()).expect("a plan");
    assert_eq!(graphar_rdf::rml(&plan), mapping(&schema()).expect("RML"));
    // The plan in the algebra: the tagged column is the one extension the algebra lacks.
    let algebra = plan.to_string();
    assert!(algebra.contains("toLangLiteral(value, lang)"), "{algebra}");
    assert!(algebra.starts_with("∪("));

    let languages = [(
        "Order/labels".to_owned(),
        ["en", "es"].map(String::from).into(),
    )]
    .into();
    let r2rml = graphar_rdf::r2rml(&plan, &languages);
    let triples: Vec<_> = TurtleParser::new()
        .with_base_iri("http://example.org/mapping.r2rml.ttl")
        .expect("a base")
        .for_slice(r2rml.as_bytes())
        .collect::<Result<_, _>>()
        .expect("R2RML is Turtle");
    let language = |tag: &str| {
        triples.iter().any(|t| {
            t.predicate.as_str() == "http://www.w3.org/ns/r2rml#language"
                && matches!(&t.object, Term::Literal(l) if l.value() == tag)
        })
    };
    assert!(language("en") && language("es"));
    assert!(r2rml.contains("WHERE q.\\\"lang\\\" = 'es'"), "{r2rml}");
    // `note` holds no language here, so it has no triples map: it would read nothing.
    assert!(!r2rml.contains("Order/note"));
    assert!(!r2rml.contains("languageMap"));
}

#[test]
fn a_triples_map_is_named_by_the_graphar_names_it_reads_and_says_its_shape() {
    let mut s = schema();
    s.vertices[1].shape = Some("https://example.org/shapes/Customer".to_owned());
    let text = mapping(&s).expect("a mapping");
    let triples: Vec<_> = TurtleParser::new()
        .with_base_iri("http://example.org/mapping.ttl")
        .expect("a base")
        .for_slice(text.as_bytes())
        .collect::<Result<_, _>>()
        .expect("Turtle");
    let maps: Vec<&str> = triples
        .iter()
        .filter(|t| matches!(&t.object, Term::NamedNode(n) if n.as_str() == "http://w3id.org/rml/TriplesMap"))
        .map(|t| match &t.subject {
            oxrdf::NamedOrBlankNode::NamedNode(n) => n.as_str(),
            other => panic!("{other}"),
        })
        .collect();
    let base = "http://example.org/mapping.ttl#";
    assert_eq!(
        maps,
        [
            "Order",
            "Order/labels",
            "Order/note",
            "Customer",
            "Order_placedBy_Customer"
        ]
        .map(|m| format!("{base}{m}")),
        "{text}"
    );
    let conforms: Vec<(String, String)> = triples
        .iter()
        .filter(|t| t.predicate.as_str() == graphar_rdf::DCT_CONFORMS_TO)
        .map(|t| (t.subject.to_string(), t.object.to_string()))
        .collect();
    assert_eq!(
        conforms,
        [(
            format!("<{base}Customer>"),
            "<https://example.org/shapes/Customer>".to_owned()
        )]
    );
    // A type with a shape has its map even with no class and no single-valued column.
    s.vertices[1].class = None;
    assert!(
        mapping(&s)
            .expect("a mapping")
            .contains("<#Customer> a rml:TriplesMap")
    );
    s.vertices[1].shape = None;
    assert!(!mapping(&s).expect("a mapping").contains("<#Customer>"));
}
