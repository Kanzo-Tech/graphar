//! Statement R: the triples [`graphar_rdf::read`] reads from a written graph are the triples
//! [`graphar_rdf::write`] was given, up to canonical lexical form, whether the subjects are typed
//! by validation or by a shape map the caller states; and a graph that does not conform is refused,
//! with nothing written.

use std::collections::BTreeSet;

use futures::executor::block_on;
use std::collections::HashMap;

use graphar_rdf::{Cardinality, Error, Object, Options, ShapeMap, Typing, read, shapes, write};
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

fn round_trip(typing: &Typing<'_>, chunk_size: u64) -> (graphar_rdf::Written, BTreeSet<String>) {
    let store = InMemory::new();
    let root = Path::from("people");
    let options = Options {
        name: "people".to_owned(),
        chunk_size,
        ..Options::default()
    };
    let written = block_on(write(&store, &root, typing, people(), &options))
        .unwrap_or_else(|e| panic!("written: {e}"));
    let back = block_on(read(&store, &root, &written.info, typing.schema())).expect("read");
    let back_set = set(back.clone());
    assert_eq!(back.len(), back_set.len(), "no triple is read twice");
    (written, back_set)
}

#[test]
fn shacl_and_shex_make_one_schema() {
    let shacl = shapes::shacl::from_turtle(&data("people.shacl.ttl")).expect("SHACL");
    let shexj = shapes::shex::from_json(&data("people.shex.json")).expect("ShExJ");
    let shexc = shapes::shex::from_compact(&data("people.shex")).expect("ShExC");
    let (shacl, shexj, shexc) = (shacl.schema(), shexj.schema(), shexc.schema());
    // ShEx `max` defaults to 1, SHACL's to unbounded: the documents say the same.
    assert_eq!(shacl, shexj);
    assert_eq!(shacl, shexc);
    // A type is a shape, named by it; the class it fixes is no column.
    assert_eq!(
        shacl
            .vertices
            .iter()
            .map(|v| (v.label.as_str(), v.class.as_deref()))
            .collect::<Vec<_>>(),
        [
            ("Organization", Some("https://example.org/Organization")),
            ("Person", Some("https://example.org/Person")),
        ]
    );
    let person = shacl.vertex("Person").expect("Person");
    assert!(
        person
            .columns
            .iter()
            .all(|c| c.predicate != graphar_rdf::RDF_TYPE)
    );
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
fn every_triple_comes_back_at_every_chunk_size_validated_or_assumed() {
    let shacl = shapes::shacl::from_turtle(&data("people.shacl.ttl")).expect("SHACL");
    let shex = shapes::shex::from_compact(&data("people.shex")).expect("ShExC");
    let nodes: HashMap<String, String> = ["alice", "bob", "carol", "dave"]
        .map(|p| (format!("https://example.org/{p}"), "Person".to_owned()))
        .into_iter()
        .chain([(
            "https://example.org/acme".to_owned(),
            "Organization".to_owned(),
        )])
        .collect();
    let (by_class, by_node) = (ShapeMap::ByClass, ShapeMap::Nodes(nodes));
    let given = canonical();
    assert_ne!(given, set(people()), "the data has non-canonical forms");
    for (name, typing) in [
        ("SHACL", Typing::Validate(&shacl)),
        ("ShEx", Typing::Validate(&shex)),
        ("by class", Typing::Assume(shacl.schema(), &by_class)),
        ("by node", Typing::Assume(shacl.schema(), &by_node)),
    ] {
        for chunk_size in [1, 2, 3, 1 << 20] {
            let (written, back) = round_trip(&typing, chunk_size);
            let missing: Vec<_> = given.difference(&back).collect();
            let extra: Vec<_> = back.difference(&given).collect();
            assert!(
                missing.is_empty() && extra.is_empty(),
                "{name}, chunk {chunk_size}: missing {missing:#?}, extra {extra:#?}"
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
}

#[test]
fn rdf_type_is_a_predicate_like_any_other() {
    // No shape fixes the class: `rdf:type` is a set of IRIs, stored and read back.
    let shapes = shapes::shex::from_compact(
        "PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#>
         PREFIX xsd: <http://www.w3.org/2001/XMLSchema#>
         <https://example.org/shapes/Thing> { rdf:type IRI * ; <https://example.org/p> xsd:string ? }",
    )
    .expect("ShExC");
    let thing = &shapes.schema().vertices[0];
    assert_eq!(
        (thing.label.as_str(), thing.class.as_deref()),
        ("Thing", None)
    );
    assert_eq!(thing.columns[0].predicate, graphar_rdf::RDF_TYPE);
    let triples: Vec<Triple> = TurtleParser::new()
        .for_slice(
            b"@prefix ex: <https://example.org/> .
              ex:a a ex:A, ex:B ; ex:p \"x\" .
              ex:b ex:p \"y\" .",
        )
        .collect::<Result<_, _>>()
        .expect("Turtle");
    let store = InMemory::new();
    let root = Path::from("t");
    let typing = Typing::Validate(&shapes);
    let written = block_on(write(
        &store,
        &root,
        &typing,
        triples.clone(),
        &Options::default(),
    ))
    .expect("written");
    assert_eq!(written.vertices, [("Thing".to_owned(), 2)]);
    let back = block_on(read(&store, &root, &written.info, shapes.schema())).expect("read");
    assert_eq!(set(back), set(triples));
}

/// How a graph is refused: a subject typed no shape, or more than one, or a triple with no place.
#[derive(Debug, PartialEq)]
enum Refused {
    Typing(&'static str),
    Unplaced(&'static str),
}

#[test]
fn a_graph_that_does_not_conform_is_refused_and_nothing_written() {
    let shapes = shapes::shacl::from_turtle(&data("people.shacl.ttl")).expect("SHACL");
    let by_class = ShapeMap::ByClass;
    let ex = "https://example.org/";
    let foaf = "http://xmlns.com/foaf/0.1/name";
    let xsd = "http://www.w3.org/2001/XMLSchema#";
    let rdf_type = graphar_rdf::RDF_TYPE;
    use Refused::{Typing as T, Unplaced as U};
    // The extra triple; how a typing by class refuses it, where only placement validates; and how
    // validation refuses it, which validates every constraint of the shapes, read closed. Validation
    // is the maximal valid shape map, so a subject that fails takes every subject referring to it
    // through `sh:node` with it, and the refusal names the first of them in IRI order: alice knows
    // bob and carol, and bob knows alice.
    for (extra, assumed, validated) in [
        (
            format!("<{ex}bob> <{rdf_type}> <{ex}Employee>"),
            U("Person has no column or edge of it"),
            T("https://example.org/alice"),
        ),
        (
            format!("<{ex}x> <{rdf_type}> <{ex}Thing>"),
            T("https://example.org/x"),
            T("https://example.org/x"),
        ),
        (
            format!("<{ex}notyped> <{foaf}> \"Nobody\""),
            T("https://example.org/notyped"),
            T("https://example.org/notyped"),
        ),
        (
            format!("_:x <{foaf}> \"Anonymous\""),
            U("its subject is a blank node"),
            U("its subject is a blank node"),
        ),
        (
            format!("<{ex}carol> <{foaf}> \"Caroline\""),
            U("Person.name has at most one value"),
            T("https://example.org/alice"),
        ),
        (
            format!("<{ex}alice> <{ex}knows> <{ex}nobody>"),
            U("its object is not a row of Person"),
            T("https://example.org/alice"),
        ),
        (
            format!("<{ex}alice> <{ex}knows> _:b"),
            U("its object is not a row of Person"),
            T("https://example.org/alice"),
        ),
        (
            format!("<{ex}bob> <{ex}shoeSize> \"42.5\"^^<{xsd}decimal>"),
            U("Person has no column or edge of it"),
            T("https://example.org/bob"),
        ),
        (
            format!("<{ex}dave> <{ex}born> \"1985-13-01\"^^<{xsd}date>"),
            U("not a value of Person.born"),
            // rudof takes an ill-typed literal for a value of its datatype, which `sh:datatype`
            // does not (SHACL 4.1.1); placement refuses it.
            U("not a value of Person.born"),
        ),
        (
            format!("<{ex}acme> <{ex}founded> \"1999\"^^<{xsd}integer>"),
            U("not a value of Organization.founded"),
            T("https://example.org/acme"),
        ),
    ] {
        let line = format!("{extra} .\n");
        let t: Triple = NTriplesParser::new()
            .for_slice(line.as_bytes())
            .next()
            .expect("a triple")
            .expect("N-Triples");
        for (typing, expected) in [
            (Typing::Assume(shapes.schema(), &by_class), assumed),
            (Typing::Validate(&shapes), validated),
        ] {
            let store = InMemory::new();
            let triples = people().into_iter().chain([t.clone()]);
            let err = block_on(write(
                &store,
                &Path::from("g"),
                &typing,
                triples,
                &Options::default(),
            ))
            .expect_err(&extra);
            let got = match &err {
                Error::Unplaced { reason, .. } => U(Box::leak(reason.clone().into_boxed_str())),
                Error::Typing { subject, .. } => T(Box::leak(subject.clone().into_boxed_str())),
                other => panic!("{extra}: {other}"),
            };
            assert_eq!(got, expected, "{extra}: {err}");
            let written: Vec<_> = block_on(futures::TryStreamExt::try_collect::<Vec<_>>(
                store.list(None),
            ))
            .expect("a listing");
            assert!(written.is_empty(), "{extra}: wrote {written:?}");
        }
    }
}

#[test]
fn validation_says_why_a_subject_has_no_type() {
    let shapes = shapes::shacl::from_turtle(&data("people.shacl.ttl")).expect("SHACL");
    let line = "<https://example.org/carol> <http://xmlns.com/foaf/0.1/name> \"Caroline\" .\n";
    let t: Triple = NTriplesParser::new()
        .for_slice(line.as_bytes())
        .next()
        .expect("a triple")
        .expect("N-Triples");
    let err = block_on(write(
        &InMemory::new(),
        &Path::from("g"),
        &Typing::Validate(&shapes),
        people().into_iter().chain([t]),
        &Options::default(),
    ))
    .expect_err("two names");
    let Error::Typing { types, reasons, .. } = &err else {
        panic!("{err}")
    };
    assert!(types.is_empty());
    // Person refuses the second name (maxCount), Organization the class.
    assert_eq!(reasons.len(), 2, "{reasons:#?}");
    assert!(
        reasons
            .iter()
            .any(|r| r.starts_with("https://example.org/shapes/Person: ")),
        "{reasons:#?}"
    );
}

#[test]
fn a_property_the_shape_forbids_is_refused() {
    let shacl = shapes::shacl::from_turtle(
        "@prefix sh: <http://www.w3.org/ns/shacl#> .
         @prefix xsd: <http://www.w3.org/2001/XMLSchema#> .
         <https://example.org/shapes/Thing> a sh:NodeShape ;
           sh:property [ sh:path <https://example.org/p> ; sh:datatype xsd:string ; sh:maxCount 0 ] .",
    );
    let shex = shapes::shex::from_compact(
        "PREFIX xsd: <http://www.w3.org/2001/XMLSchema#>
         <https://example.org/shapes/Thing> { <https://example.org/p> xsd:string {0,0} }",
    );
    for (name, refused) in [("SHACL", shacl), ("ShEx", shex)] {
        let Err(Error::Shapes(reason)) = refused else {
            panic!("{name}: a property with a maximum of 0 is refused");
        };
        assert!(
            reason.contains("<https://example.org/shapes/Thing>")
                && reason.contains("<https://example.org/p>")
                && reason.contains("has no column"),
            "{name}: {reason}"
        );
    }
}

/// The files under `root`, by name, with their bytes.
fn files(store: &InMemory, root: &str) -> HashMap<String, Vec<u8>> {
    let listed: Vec<_> = block_on(futures::TryStreamExt::try_collect::<Vec<_>>(
        store.list(Some(&Path::from(root))),
    ))
    .expect("a listing");
    listed
        .into_iter()
        .map(|meta| {
            let bytes = block_on(async {
                object_store::ObjectStoreExt::get(store, &meta.location)
                    .await?
                    .bytes()
                    .await
            })
            .expect("the bytes");
            let name = meta.location.as_ref()[root.len() + 1..].to_owned();
            (name, bytes.to_vec())
        })
        .collect()
}

#[test]
fn the_shape_document_is_saved_as_it_was_read_and_only_when_validated() {
    let shacl = data("people.shacl.ttl");
    let shexc = data("people.shex");
    let shexj = data("people.shex.json");
    let by_class = ShapeMap::ByClass;
    let documents = [
        (
            shapes::shacl::from_turtle(&shacl).expect("SHACL"),
            &shacl,
            graphar_rdf::SHACL_FILE,
        ),
        (
            shapes::shex::from_compact(&shexc).expect("ShExC"),
            &shexc,
            graphar_rdf::SHEXC_FILE,
        ),
        (
            shapes::shex::from_json(&shexj).expect("ShExJ"),
            &shexj,
            graphar_rdf::SHEXJ_FILE,
        ),
    ];
    let all = [
        graphar_rdf::SHACL_FILE,
        graphar_rdf::SHEXC_FILE,
        graphar_rdf::SHEXJ_FILE,
    ];
    for (shapes, text, file) in &documents {
        assert_eq!(shapes.syntax().file(), *file);
        let store = InMemory::new();
        let written = block_on(write(
            &store,
            &Path::from("g"),
            &Typing::Validate(shapes),
            people(),
            &Options::default(),
        ))
        .expect("written");
        let files = files(&store, "g");
        assert_eq!(
            files.get(*file).map(Vec::as_slice),
            Some(text.as_bytes()),
            "{file}"
        );
        assert!(
            all.iter()
                .filter(|f| *f != file)
                .all(|f| !files.contains_key(*f))
        );
        assert_eq!(
            written.info.extra_info,
            [(graphar_rdf::SHAPES_KEY.to_owned(), (*file).to_owned())]
        );
        let graph = String::from_utf8(files["graph.graph.yml"].clone()).expect("UTF-8");
        assert!(graph.contains(file), "{graph}");
        // Each type's triples map says the shape its subjects conform to.
        let mapping = String::from_utf8(files[graphar_rdf::MAPPING_FILE].clone()).expect("UTF-8");
        for shape in ["Person", "Organization"] {
            assert!(
                mapping.contains(&format!(
                    "<#{shape}> a rml:TriplesMap ;\n  rml:logicalSource [ rml:source [ a rml:Source ] ; rml:referenceFormulation rml:SQL2008Table ; rml:iterator \"\\\"{shape}\\\"\" ] ;"
                )),
                "{mapping}"
            );
            assert!(
                mapping.contains(&format!(
                    "dct:conformsTo <https://example.org/shapes/{shape}>"
                )),
                "{mapping}"
            );
        }
    }
    let store = InMemory::new();
    let written = block_on(write(
        &store,
        &Path::from("g"),
        &Typing::Assume(documents[0].0.schema(), &by_class),
        people(),
        &Options::default(),
    ))
    .expect("written");
    let files = files(&store, "g");
    assert!(
        all.iter().all(|f| !files.contains_key(*f)),
        "{:?}",
        files.keys()
    );
    assert!(written.info.extra_info.is_empty());
    assert!(files.contains_key(graphar_rdf::MAPPING_FILE));
}
