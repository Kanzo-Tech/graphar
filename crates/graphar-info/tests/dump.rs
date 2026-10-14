//! The bytes of our own output: a dump is the same text every time, block style, keys in the order
//! GraphAr's C++ `Dump` writes them.

use graphar_info::{
    AdjListType, AdjacentList, Cardinality, DataType, EdgeInfo, FileType, GraphInfo, InfoVersion,
    Property, PropertyGroup, VertexInfo,
};

fn property(name: &str, data_type: DataType, is_primary: bool) -> Property {
    Property {
        name: name.to_owned(),
        data_type,
        is_primary,
        is_nullable: !is_primary,
        cardinality: Cardinality::Single,
    }
}

fn graph() -> GraphInfo {
    let person = VertexInfo {
        r#type: "Person".to_owned(),
        chunk_size: 5,
        prefix: "vertex/Person/".to_owned(),
        labels: Vec::new(),
        property_groups: vec![PropertyGroup {
            properties: vec![
                property("subject", DataType::String, true),
                Property {
                    is_nullable: false,
                    ..property("name", DataType::String, false)
                },
                Property {
                    cardinality: Cardinality::List,
                    ..property("nickname", DataType::String, false)
                },
            ],
            file_type: FileType::Parquet,
            prefix: "subject_name/".to_owned(),
        }],
        version: InfoVersion::default(),
    };
    let knows = EdgeInfo {
        src_type: "Person".to_owned(),
        edge_type: "knows".to_owned(),
        dst_type: "Person".to_owned(),
        chunk_size: 1024,
        src_chunk_size: 5,
        dst_chunk_size: 5,
        directed: true,
        prefix: "edge/Person_knows_Person/".to_owned(),
        adj_lists: vec![AdjacentList {
            r#type: AdjListType::OrderedBySource,
            file_type: FileType::Parquet,
            prefix: "ordered_by_source/".to_owned(),
        }],
        property_groups: vec![PropertyGroup {
            properties: vec![property("since", DataType::Date, false)],
            file_type: FileType::Parquet,
            prefix: "since/".to_owned(),
        }],
        version: InfoVersion::default(),
    };
    GraphInfo {
        name: "fossil".to_owned(),
        prefix: Some("./".to_owned()),
        vertices: vec![person],
        edges: vec![knows],
        labels: Vec::new(),
        version: InfoVersion::default(),
        extra_info: vec![
            ("rml".to_owned(), "mapping.ttl".to_owned()),
            ("shacl".to_owned(), "shapes.ttl".to_owned()),
        ],
    }
}

const PERSON: &str = "\
type: Person
chunk_size: 5
prefix: vertex/Person/
property_groups:
- prefix: subject_name/
  file_type: parquet
  properties:
  - name: subject
    data_type: string
    is_primary: true
    is_nullable: false
  - name: name
    data_type: string
    is_primary: false
    is_nullable: false
  - name: nickname
    data_type: string
    is_primary: false
    is_nullable: true
    cardinality: list
version: gar/v1
";

const KNOWS: &str = "\
src_type: Person
edge_type: knows
dst_type: Person
chunk_size: 1024
src_chunk_size: 5
dst_chunk_size: 5
prefix: edge/Person_knows_Person/
directed: true
adj_lists:
- ordered: true
  aligned_by: src
  prefix: ordered_by_source/
  file_type: parquet
property_groups:
- prefix: since/
  file_type: parquet
  properties:
  - name: since
    data_type: date
    is_primary: false
    is_nullable: true
version: gar/v1
";

const FOSSIL: &str = "\
name: fossil
prefix: ./
vertices:
- Person.vertex.yml
edges:
- Person_knows_Person.edge.yml
version: gar/v1
extra_info:
- key: rml
  value: mapping.ttl
- key: shacl
  value: shapes.ttl
";

#[test]
fn a_dump_is_these_bytes() {
    let files = graph().dump().unwrap();
    let names: Vec<_> = files.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(
        names,
        [
            "Person.vertex.yml",
            "Person_knows_Person.edge.yml",
            "fossil.graph.yml"
        ]
    );
    assert_eq!(files[0].1, PERSON);
    assert_eq!(files[1].1, KNOWS);
    assert_eq!(files[2].1, FOSSIL);
}

#[test]
fn the_bytes_load_back_to_the_model() {
    let loaded = GraphInfo::load(FOSSIL, |name| {
        Ok(match name {
            "Person.vertex.yml" => PERSON,
            "Person_knows_Person.edge.yml" => KNOWS,
            _ => panic!("{name}"),
        }
        .to_owned())
    })
    .unwrap();
    assert_eq!(loaded, graph());
}

#[test]
fn types_and_versions_are_written_in_graphars_words() {
    for name in [
        "bool",
        "int32",
        "int64",
        "float",
        "double",
        "string",
        "date",
        "timestamp",
        "time",
        "list<float>",
        "list<list<int64>>",
        "point",
    ] {
        assert_eq!(name.parse::<DataType>().unwrap().to_string(), name);
    }
    let v: InfoVersion = "gar/v1 (point, polygon)".parse().unwrap();
    assert_eq!(v.user_define_types, ["point", "polygon"]);
    assert_eq!(v.to_string(), "gar/v1 (point,polygon)");
    assert!("gar/v2".parse::<InfoVersion>().is_err());
    assert!("v1".parse::<InfoVersion>().is_err());
}
