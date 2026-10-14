//! Each rule `validate` holds, broken once on a graph that is otherwise valid.

use graphar_info::{AdjListType, Cardinality, DataType, Error, FileType, GraphInfo, InfoVersion};

const GRAPH: &str = "\
name: g
vertices:
- person.vertex.yml
edges:
- person_knows_person.edge.yml
version: gar/v1
";

const PERSON: &str = "\
type: person
chunk_size: 100
prefix: vertex/person/
property_groups:
- file_type: parquet
  properties:
  - name: id
    data_type: int64
    is_primary: true
  - name: name
    data_type: string
version: gar/v1
";

const KNOWS: &str = "\
src_type: person
edge_type: knows
dst_type: person
chunk_size: 1024
src_chunk_size: 100
dst_chunk_size: 100
directed: true
adj_lists:
- ordered: true
  aligned_by: src
  file_type: parquet
property_groups:
- file_type: parquet
  properties:
  - name: since
    data_type: date
version: gar/v1
";

fn graph() -> GraphInfo {
    let g = GraphInfo::load(GRAPH, |name| {
        Ok(match name {
            "person.vertex.yml" => PERSON,
            _ => KNOWS,
        }
        .to_owned())
    })
    .unwrap();
    g.validate().unwrap();
    g
}

#[track_caller]
fn refused(g: GraphInfo, reason: &str) {
    match g.validate() {
        Err(Error::Invalid { at, reason: r }) => {
            assert!(r.contains(reason), "{at}: {r}");
            assert!(
                matches!(g.dump(), Err(Error::Invalid { .. })),
                "dump validates first"
            );
        }
        other => panic!("expected `{reason}`, got {other:?}"),
    }
}

#[test]
fn names_and_sizes() {
    let mut g = graph();
    g.name.clear();
    refused(g, "`name` is empty");
    let mut g = graph();
    g.prefix = Some(String::new());
    refused(g, "`prefix` is empty");
    let mut g = graph();
    g.vertices[0].chunk_size = 0;
    refused(g, "`chunk_size` is 0");
    let mut g = graph();
    g.edges[0].src_chunk_size = 0;
    refused(g, "`src_chunk_size` is 0");
    let mut g = graph();
    g.edges[0].prefix.clear();
    refused(g, "`prefix` is empty");
}

#[test]
fn a_type_is_listed_once() {
    let mut g = graph();
    g.vertices.push(g.vertices[0].clone());
    refused(g, "vertex type `person` is listed twice");
    let mut g = graph();
    g.edges.push(g.edges[0].clone());
    refused(g, "edge type `person_knows_person` is listed twice");
    let mut g = graph();
    let again = g.edges[0].adj_lists[0].clone();
    g.edges[0].adj_lists.push(again);
    refused(g, "adjacency list `ordered_by_source` is listed twice");
    let mut g = graph();
    g.edges[0].adj_lists.clear();
    refused(g, "`adj_lists` is empty");
}

#[test]
fn an_edge_joins_vertex_types_of_the_graph_at_their_chunk_size() {
    let mut g = graph();
    g.edges[0].dst_type = "post".to_owned();
    refused(g, "`dst_type` `post` is not a vertex type of the graph");
    let mut g = graph();
    g.edges[0].src_chunk_size = 50;
    refused(
        g,
        "`src_chunk_size` is 50, and vertex `person`'s `chunk_size` is 100",
    );
}

#[test]
fn properties() {
    let mut g = graph();
    let id = g.vertices[0].property_groups[0].properties[0].clone();
    g.vertices[0]
        .property_groups
        .push(graphar_info::PropertyGroup {
            properties: vec![id],
            file_type: FileType::Parquet,
            prefix: "id2/".to_owned(),
        });
    refused(g, "the name is used twice in this type");

    let mut g = graph();
    g.vertices[0].property_groups[0].properties.clear();
    refused(g, "`properties` is empty");

    let mut g = graph();
    g.vertices[0].property_groups[0].properties[0].is_nullable = true;
    refused(g, "a primary key cannot be nullable");

    let mut g = graph();
    g.edges[0].property_groups[0].properties[0].cardinality = Cardinality::Set;
    refused(g, "an edge property's cardinality must be `single`");

    let mut g = graph();
    g.vertices[0].property_groups[0].file_type = FileType::Csv;
    g.vertices[0].property_groups[0].properties[1].data_type =
        DataType::List(Box::new(DataType::String));
    refused(g, "a `list` type cannot be stored in `csv`");

    let mut g = graph();
    g.vertices[0].property_groups[0].file_type = FileType::Csv;
    g.vertices[0].property_groups[0].properties[1].cardinality = Cardinality::List;
    refused(
        g,
        "a cardinality other than `single` cannot be stored in `csv`",
    );
}

#[test]
fn a_user_defined_type_is_declared_by_the_version() {
    let mut g = graph();
    g.vertices[0].property_groups[0].properties[1].data_type = "point".parse().unwrap();
    refused(
        g.clone(),
        "data type `point` is not GraphAr's, and the version does not declare it",
    );
    g.vertices[0].version = InfoVersion {
        version: 1,
        user_define_types: vec!["point".to_owned()],
    };
    g.validate().unwrap();
}

#[test]
fn extra_info_keys_are_unique() {
    let mut g = graph();
    g.extra_info = vec![("k".into(), "a".into()), ("k".into(), "b".into())];
    refused(g, "`extra_info` key `k` is repeated");
}

#[test]
fn a_document_that_is_not_graphars_is_refused_at_load() {
    for (doc, why) in [
        ("name: g\nversion: gar/v2\n", "unsupported version `gar/v2`"),
        (
            "name: g\nversion: gar/v1\nformat: x\n",
            "unknown field `format`",
        ),
        ("version: gar/v1\n", "missing field `name`"),
    ] {
        let err = GraphInfo::load(doc, |_| unreachable!()).unwrap_err();
        assert!(
            matches!(err, Error::Yaml { .. }) && err.to_string().contains(why),
            "{err}"
        );
    }
    let edge = KNOWS.replace("aligned_by: src", "aligned_by: source");
    let err = graphar_info::EdgeInfo::load(&edge).unwrap_err();
    assert!(
        err.to_string().contains("unknown variant `source`"),
        "{err}"
    );
    let edge = KNOWS.replace(
        "file_type: parquet\nproperty_groups",
        "file_type: avro\nproperty_groups",
    );
    let err = graphar_info::EdgeInfo::load(&edge).unwrap_err();
    assert!(
        err.to_string().contains("unknown file type `avro`"),
        "{err}"
    );
    assert_eq!(
        graph().edges[0].adj_lists[0].r#type,
        AdjListType::OrderedBySource
    );
}
