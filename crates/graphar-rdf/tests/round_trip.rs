//! Statement R: the triples [`graphar_rdf::read`] reads from a written graph are the triples
//! [`graphar_rdf::write`] was given, up to canonical lexical form; and a graph that does not
//! conform is refused, with nothing written.

use std::collections::BTreeSet;

use futures::executor::block_on;
use graphar_rdf::{Cardinality, Error, Object, Options, Schema, read, shapes, write};
use object_store::ObjectStore;
use object_store::{memory::InMemory, path::Path};
use oxrdf::Triple;
use oxttl::{NTriplesParser, TurtleParser};

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

/// `people.ttl` as it reads back: bob's age and `active` in their canonical forms.
fn canonical() -> BTreeSet<String> {
    set(people())
        .into_iter()
        .map(|t| {
            t.replace(r#""031"^^"#, r#""31"^^"#).replace(
                r#""0"^^<http://www.w3.org/2001/XMLSchema#boolean>"#,
                r#""false"^^<http://www.w3.org/2001/XMLSchema#boolean>"#,
            )
        })
        .collect()
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
    let shexj = shapes::shex::from_json(&data("people.shex.json")).expect("ShExJ");
    let shexc = shapes::shex::from_compact(&data("people.shex")).expect("ShExC");
    // ShEx `max` defaults to 1, SHACL's to unbounded: the documents say the same.
    assert_eq!(shacl, shexj);
    assert_eq!(shacl, shexc);
    let person = shacl.vertex("Person").expect("Person");
    let names: Vec<(&str, &Object, Cardinality)> = person
        .columns
        .iter()
        .map(|c| (c.name.as_str(), &c.object, c.cardinality))
        .collect();
    let named = |predicate: &str| {
        let c = person.columns.iter().find(|c| c.predicate == predicate);
        c.map(|c| c.name.as_str())
    };
    // `http://xmlns.com/…` before `https://schema.org/…`: the first takes the name.
    assert_eq!(named("http://xmlns.com/foaf/0.1/name"), Some("name"));
    assert_eq!(named("https://schema.org/name"), Some("name_2"));
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
    let given = canonical();
    assert_ne!(given, set(people()), "the data has non-canonical forms");
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
            [("Organization".to_owned(), 1), ("Person".to_owned(), 4)]
        );
        // alice→bob, alice→carol, bob→alice, carol→dave.
        assert_eq!(written.edges[0], ("Person_knows_Person".to_owned(), 4));
        assert_eq!(
            written.edges[1],
            ("Person_worksFor_Organization".to_owned(), 2)
        );
    }
}

#[test]
fn a_graph_that_does_not_conform_is_refused_and_nothing_written() {
    let schema = shapes::shacl::from_turtle(&data("people.shacl.ttl")).expect("SHACL");
    let ex = "https://example.org/";
    let foaf = "http://xmlns.com/foaf/0.1/name";
    let xsd = "http://www.w3.org/2001/XMLSchema#";
    let rdf_type = graphar_rdf::RDF_TYPE;
    for (extra, reason) in [
        (
            format!("<{ex}bob> <{rdf_type}> <{ex}Employee>"),
            "its subject has a second rdf:type",
        ),
        (
            format!("<{ex}x> <{rdf_type}> <{ex}Thing>"),
            "no shape targets this class",
        ),
        (
            format!("<{ex}notyped> <{foaf}> \"Nobody\""),
            "its subject has no rdf:type",
        ),
        (
            format!("_:x <{foaf}> \"Anonymous\""),
            "its subject is a blank node",
        ),
        (
            format!("<{ex}carol> <{foaf}> \"Caroline\""),
            "Person.name has at most one value",
        ),
        (
            format!("<{ex}alice> <{ex}knows> <{ex}nobody>"),
            "its object is not a row of Person",
        ),
        (
            format!("<{ex}alice> <{ex}knows> _:b"),
            "its object is not a row of Person",
        ),
        (
            format!("<{ex}bob> <{ex}shoeSize> \"42.5\"^^<{xsd}decimal>"),
            "Person has no column or edge of it",
        ),
        (
            format!("<{ex}dave> <{ex}born> \"1985-13-01\"^^<{xsd}date>"),
            "not a value of Person.born",
        ),
        (
            format!("<{ex}acme> <{ex}founded> \"1999\"^^<{xsd}integer>"),
            "not a value of Organization.founded",
        ),
    ] {
        let line = format!("{extra} .\n");
        let t: Triple = NTriplesParser::new()
            .for_slice(line.as_bytes())
            .next()
            .expect("a triple")
            .expect("N-Triples");
        let store = InMemory::new();
        let triples = people().into_iter().chain([t]);
        let err = block_on(write(
            &store,
            &Path::from("g"),
            &schema,
            triples,
            &Options::default(),
        ))
        .expect_err(&extra);
        let Error::Unplaced { reason: why, .. } = &err else {
            panic!("{extra}: {err}");
        };
        assert_eq!(why, reason, "{extra}");
        let written: Vec<_> = block_on(futures::TryStreamExt::try_collect::<Vec<_>>(
            store.list(None),
        ))
        .expect("a listing");
        assert!(written.is_empty(), "{extra}: wrote {written:?}");
    }
}
