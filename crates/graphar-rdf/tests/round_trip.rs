//! Statement R: the triples [`graphar_rdf::read`] reads from a written graph are the triples
//! [`graphar_rdf::write`] was given, whatever the shapes type.

use std::collections::BTreeSet;

use futures::executor::block_on;
use graphar_rdf::{Cardinality, Object, Options, Schema, read, shapes, write};
use object_store::{memory::InMemory, path::Path};
use oxrdf::Triple;
use oxttl::TurtleParser;

fn data(name: &str) -> String {
    std::fs::read_to_string(format!("{}/tests/data/{name}", env!("CARGO_MANIFEST_DIR")))
        .expect("the data")
}

/// Parsed once: a parser labels each `[]` afresh.
fn people() -> Vec<Triple> {
    static PEOPLE: std::sync::LazyLock<Vec<Triple>> = std::sync::LazyLock::new(|| {
        TurtleParser::new()
            .for_slice(data("people.ttl").as_bytes())
            .collect::<Result<_, _>>()
            .expect("Turtle")
    });
    PEOPLE.clone()
}

fn set(triples: impl IntoIterator<Item = Triple>) -> BTreeSet<String> {
    triples.into_iter().map(|t| t.to_string()).collect()
}

fn round_trip(schema: &Schema, chunk_size: u64) -> (graphar_rdf::Written, BTreeSet<String>) {
    let store = InMemory::new();
    let root = Path::from("people");
    let options = Options {
        name: "people".to_owned(),
        chunk_size,
        ..Options::default()
    };
    let written = block_on(write(&store, &root, schema, people(), &options)).expect("written");
    let back = block_on(read(&store, &root, &written.info, schema)).expect("read");
    let back_set = set(back.clone());
    assert_eq!(back.len(), back_set.len(), "no triple is read twice");
    (written, back_set)
}

#[test]
fn shacl_and_shex_make_one_schema() {
    let shacl = shapes::shacl::from_turtle(&data("people.shacl.ttl")).expect("SHACL");
    let shex = shapes::shex::from_json(&data("people.shex.json")).expect("ShExJ");
    // ShEx `max` defaults to 1, SHACL's to unbounded: the documents say the same.
    assert_eq!(shacl, shex);
    let person = shacl.vertex("Person").expect("Person");
    let names: Vec<(&str, &Object, Cardinality)> = person
        .columns
        .iter()
        .map(|c| (c.name.as_str(), &c.object, c.cardinality))
        .collect();
    assert_eq!(names[0].0, "name");
    assert_eq!(names[1].0, "name_2", "schema:name beside foaf:name");
    assert!(
        names
            .iter()
            .any(|(n, o, c)| *n == "nick" && **o == Object::LangString && *c == Cardinality::Set)
    );
    let edges: Vec<String> = shacl
        .edges
        .iter()
        .map(graphar_rdf::EdgeType::triple)
        .collect();
    assert_eq!(
        edges,
        ["Person_knows_Person", "Person_worksFor_Organization"]
    );
}

#[test]
fn every_triple_comes_back_at_every_chunk_size() {
    let schema = shapes::shacl::from_turtle(&data("people.shacl.ttl")).expect("SHACL");
    let given = set(people());
    for chunk_size in [1, 2, 3, 1 << 20] {
        let (written, back) = round_trip(&schema, chunk_size);
        let missing: Vec<_> = given.difference(&back).collect();
        let extra: Vec<_> = back.difference(&given).collect();
        assert!(
            missing.is_empty() && extra.is_empty(),
            "chunk {chunk_size}: missing {missing:#?}, extra {extra:#?}"
        );
        assert_eq!(written.triples as usize, given.len());
        assert_eq!(
            written.vertices,
            [("Person".to_owned(), 4), ("Organization".to_owned(), 1)]
        );
        // alice→bob, alice→carol, bob→alice; alice→nobody (no row) and carol→_:b stay triples.
        assert_eq!(written.edges[0], ("Person_knows_Person".to_owned(), 3));
        assert_eq!(
            written.edges[1],
            ("Person_worksFor_Organization".to_owned(), 2)
        );
    }
}

#[test]
fn what_no_column_holds_exactly_stays_a_triple() {
    let schema = shapes::shacl::from_turtle(&data("people.shacl.ttl")).expect("SHACL");
    let (written, _) = round_trip(&schema, 2);
    // bob: ex:Employee, "031", the impossible date, the shoe size; carol: two foaf:names, the
    // blank friend and what it says; alice: ex:nobody; _:x's two triples; ex:notyped's one; and
    // acme's founding year, an `xsd:integer` where the shape says `xsd:int`.
    assert_eq!(written.residual, 4 + 2 + 2 + 1 + 2 + 1 + 1);
}

#[test]
fn with_no_shapes_every_triple_is_in_the_residual_table() {
    let (written, back) = round_trip(&Schema::default(), 4);
    assert_eq!(back, set(people()));
    assert_eq!(written.residual, written.triples);
}
