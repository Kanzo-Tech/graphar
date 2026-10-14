//! The sample graph: what the tests write and read back, what `examples/sample.rs` writes to
//! `conformance/sample/`, and what GraphAr's own C++ reader and `graphar-duckdb` open there.
//!
//! It is small and covers what a reader can get wrong: several chunks per vertex type, two
//! property groups on one type, every built-in type C++ reads (`time` and `list` have no reader in
//! C++'s `DataTypeToArrowDataType`), a null, a vertex type with no primary key, an edge type with
//! two adjacency lists and a property group, and an empty part.

#![allow(dead_code, missing_docs)]

use std::collections::BTreeMap;
use std::sync::Arc;

use arrow_array::{
    ArrayRef, BooleanArray, Date32Array, Float32Array, Float64Array, Int32Array, Int64Array,
    ListArray, RecordBatch, StringArray, StructArray, TimestampMillisecondArray,
};
use arrow_buffer::{NullBuffer, OffsetBuffer};
use arrow_schema::{DataType as Arrow, Field, Fields};
use graphar_info::{
    AdjListType, AdjacentList, Cardinality, DataType, EdgeInfo, FileType, GraphInfo, InfoVersion,
    Property, PropertyGroup, VertexInfo,
};
use graphar_parquet::{
    DST_INDEX_COL, Error, Options, SRC_INDEX_COL, UserTypes, write_edges, write_info,
    write_vertices,
};
use object_store::{ObjectStore, path::Path};

pub fn property(name: &str, data_type: DataType, is_primary: bool, is_nullable: bool) -> Property {
    Property {
        name: name.to_owned(),
        data_type,
        is_primary,
        is_nullable,
        cardinality: Cardinality::Single,
    }
}

pub fn group(prefix: &str, properties: Vec<Property>) -> PropertyGroup {
    PropertyGroup {
        properties,
        file_type: FileType::Parquet,
        prefix: prefix.to_owned(),
    }
}

pub fn adj_list(r#type: AdjListType) -> AdjacentList {
    AdjacentList {
        r#type,
        file_type: FileType::Parquet,
        prefix: format!("{type}/"),
    }
}

pub fn person() -> VertexInfo {
    VertexInfo {
        r#type: "person".to_owned(),
        chunk_size: 3,
        prefix: "vertex/person/".to_owned(),
        labels: Vec::new(),
        property_groups: vec![
            group("id/", vec![property("id", DataType::Int64, true, false)]),
            group(
                "name_age_born_active/",
                vec![
                    property("name", DataType::String, false, false),
                    property("age", DataType::Int32, false, true),
                    property("born", DataType::Date, false, true),
                    property("active", DataType::Bool, false, true),
                ],
            ),
        ],
        version: InfoVersion::default(),
    }
}

pub fn software() -> VertexInfo {
    VertexInfo {
        r#type: "software".to_owned(),
        chunk_size: 2,
        prefix: "vertex/software/".to_owned(),
        labels: Vec::new(),
        property_groups: vec![group(
            "id_name_price_released/",
            vec![
                property("id", DataType::Int64, true, false),
                property("name", DataType::String, false, false),
                property("price", DataType::Float, false, true),
                property("released", DataType::Timestamp, false, true),
            ],
        )],
        version: InfoVersion::default(),
    }
}

/// A vertex type with no primary key: a multi-valued property as a table, a row per value, `src`
/// the owner's internal id.
pub fn nickname() -> VertexInfo {
    VertexInfo {
        r#type: "nickname".to_owned(),
        chunk_size: 8,
        prefix: "vertex/nickname/".to_owned(),
        labels: Vec::new(),
        property_groups: vec![group(
            "src_value/",
            vec![
                property("src", DataType::Int64, false, false),
                property("value", DataType::String, false, false),
            ],
        )],
        version: InfoVersion::default(),
    }
}

pub fn knows() -> EdgeInfo {
    EdgeInfo {
        src_type: "person".to_owned(),
        edge_type: "knows".to_owned(),
        dst_type: "person".to_owned(),
        // Not 2: part 0 then holds exactly 3 chunks of 2, and C++'s high-level `EdgeIter`
        // (`graph_reader.h`, `operator++`) seeks past a part whose last chunk is full instead
        // of moving to the next part.
        chunk_size: 4,
        src_chunk_size: 3,
        dst_chunk_size: 3,
        directed: true,
        prefix: "edge/person_knows_person/".to_owned(),
        adj_lists: vec![
            adj_list(AdjListType::OrderedBySource),
            adj_list(AdjListType::UnorderedByDest),
        ],
        property_groups: vec![group(
            "weight/",
            vec![property("weight", DataType::Double, false, true)],
        )],
        version: InfoVersion::default(),
    }
}

pub fn created() -> EdgeInfo {
    EdgeInfo {
        src_type: "person".to_owned(),
        edge_type: "created".to_owned(),
        dst_type: "software".to_owned(),
        chunk_size: 4,
        src_chunk_size: 3,
        dst_chunk_size: 2,
        directed: true,
        prefix: "edge/person_created_software/".to_owned(),
        adj_lists: vec![adj_list(AdjListType::OrderedBySource)],
        property_groups: Vec::new(),
        version: InfoVersion::default(),
    }
}

pub fn graph() -> GraphInfo {
    GraphInfo {
        name: "sample".to_owned(),
        // No `prefix`: GraphAr's default, the graph document's directory. C++ resolves a written
        // `./` against the working directory, not the document.
        prefix: None,
        vertices: vec![person(), software(), nickname()],
        edges: vec![knows(), created()],
        labels: Vec::new(),
        version: InfoVersion::default(),
        extra_info: vec![("written_by".to_owned(), "graphar-parquet".to_owned())],
    }
}

fn batch(columns: Vec<(&str, ArrayRef)>) -> RecordBatch {
    RecordBatch::try_from_iter(columns).expect("a batch")
}

pub const PERSONS: [&str; 7] = ["ana", "bo", "cy", "di", "ed", "flo", "gus"];

pub fn person_rows() -> RecordBatch {
    batch(vec![
        ("id", Arc::new(Int64Array::from_iter_values(100..107))),
        ("name", Arc::new(StringArray::from(PERSONS.to_vec()))),
        (
            "age",
            Arc::new(Int32Array::from(vec![
                Some(31),
                Some(25),
                None,
                Some(40),
                Some(19),
                Some(52),
                Some(33),
            ])),
        ),
        (
            "born",
            Arc::new(Date32Array::from(vec![
                Some(0),
                Some(10_000),
                Some(-365),
                None,
                Some(12_000),
                Some(1),
                Some(7_000),
            ])),
        ),
        (
            "active",
            Arc::new(BooleanArray::from(vec![
                Some(true),
                Some(false),
                Some(true),
                Some(true),
                None,
                Some(false),
                Some(true),
            ])),
        ),
    ])
}

pub fn software_rows() -> RecordBatch {
    batch(vec![
        (
            "id",
            Arc::new(Int64Array::from_iter_values([900, 901, 902])),
        ),
        (
            "name",
            Arc::new(StringArray::from(vec!["lop", "ripple", "nebula"])),
        ),
        (
            "price",
            Arc::new(Float32Array::from(vec![Some(1.5), None, Some(0.25)])),
        ),
        (
            "released",
            Arc::new(TimestampMillisecondArray::from(vec![
                Some(1_700_000_000_123),
                Some(0),
                None,
            ])),
        ),
    ])
}

pub fn nickname_rows() -> RecordBatch {
    batch(vec![
        ("src", Arc::new(Int64Array::from_iter_values([0, 0, 3]))),
        (
            "value",
            Arc::new(StringArray::from(vec!["annie", "an", "dee"])),
        ),
    ])
}

/// `person knows person`, in no particular order. No edge starts at persons 3–5, so part 1 of the
/// list by source is empty.
pub const KNOWS: [(i64, i64, f64); 8] = [
    (0, 1, 0.5),
    (2, 6, 1.0),
    (0, 2, 0.25),
    (6, 5, 2.0),
    (1, 2, 0.75),
    (0, 6, 1.5),
    (6, 0, 3.0),
    (1, 0, 0.125),
];

pub const CREATED: [(i64, i64); 4] = [(6, 2), (0, 0), (2, 1), (6, 0)];

pub fn knows_rows() -> RecordBatch {
    batch(vec![
        (
            SRC_INDEX_COL,
            Arc::new(Int64Array::from_iter_values(KNOWS.iter().map(|e| e.0))),
        ),
        (
            DST_INDEX_COL,
            Arc::new(Int64Array::from_iter_values(KNOWS.iter().map(|e| e.1))),
        ),
        (
            "weight",
            Arc::new(Float64Array::from_iter_values(KNOWS.iter().map(|e| e.2))),
        ),
    ])
}

pub fn created_rows() -> RecordBatch {
    batch(vec![
        (
            SRC_INDEX_COL,
            Arc::new(Int64Array::from_iter_values(CREATED.iter().map(|e| e.0))),
        ),
        (
            DST_INDEX_COL,
            Arc::new(Int64Array::from_iter_values(CREATED.iter().map(|e| e.1))),
        ),
    ])
}

/// Writes the whole sample under `root`, the graph document last.
pub async fn write(store: &dyn ObjectStore, root: &Path) -> Result<(), Error> {
    write_vertices(
        store,
        root,
        &person(),
        &[person_rows()],
        &Options::default(),
    )
    .await?;
    write_vertices(
        store,
        root,
        &software(),
        &[software_rows()],
        &Options::default(),
    )
    .await?;
    write_vertices(
        store,
        root,
        &nickname(),
        &[nickname_rows()],
        &Options::default(),
    )
    .await?;
    for adj in [AdjListType::OrderedBySource, AdjListType::UnorderedByDest] {
        write_edges(
            store,
            root,
            &knows(),
            adj,
            7,
            &[knows_rows()],
            &Options::default(),
        )
        .await?;
    }
    write_edges(
        store,
        root,
        &created(),
        AdjListType::OrderedBySource,
        7,
        &[created_rows()],
        &Options::default(),
    )
    .await?;
    write_info(store, root, &graph()).await
}

// ---------------------------------------------------------------------------------------------
// `user_defined`: a graph whose one property is a list of a user-defined type. The specification
// allows it ("GraphAr also supports the user-defined data types, which can be used to represent
// complex data structures, such as the struct, map, and union types", format.md, "Property Data
// Types"); GraphAr's C++ library refuses to open it (`conformance/read_cpp.py` says how). It is a
// graph of its own so that `sample`, which C++ opens, stays the C++ reader's check.
// ---------------------------------------------------------------------------------------------

/// The user-defined type of the graph, declared by its version: `gar/v1 (langString)`.
pub const LANG_STRING: &str = "langString";

/// The Arrow type the caller gives `langString`: `STRUCT<value: STRING, lang: STRING>`.
pub fn user_types() -> UserTypes {
    let fields = Fields::from(vec![
        Field::new("value", Arrow::Utf8, false),
        Field::new("lang", Arrow::Utf8, false),
    ]);
    BTreeMap::from([(LANG_STRING.to_owned(), Arrow::Struct(fields))])
}

pub fn declaring() -> InfoVersion {
    InfoVersion {
        version: 1,
        user_define_types: vec![LANG_STRING.to_owned()],
    }
}

pub fn concept() -> VertexInfo {
    VertexInfo {
        r#type: "concept".to_owned(),
        chunk_size: 4,
        prefix: "vertex/concept/".to_owned(),
        labels: Vec::new(),
        property_groups: vec![group(
            "id_labels/",
            vec![
                property("id", DataType::Int64, true, false),
                // One level of list, from the cardinality: a non-single property is an Arrow list
                // (C++ `VertexPropertyWriter::validate` wraps it in `arrow::list`), so the type is
                // not itself a `list<…>`.
                Property {
                    cardinality: Cardinality::Set,
                    ..property(
                        "labels",
                        DataType::UserDefined(LANG_STRING.to_owned()),
                        false,
                        true,
                    )
                },
            ],
        )],
        version: declaring(),
    }
}

pub fn user_defined_graph() -> GraphInfo {
    GraphInfo {
        name: "user_defined".to_owned(),
        prefix: None,
        vertices: vec![concept()],
        edges: Vec::new(),
        labels: Vec::new(),
        version: declaring(),
        extra_info: Vec::new(),
    }
}

/// Three concepts: two labels in two languages, none, and a null list.
pub fn concept_rows() -> RecordBatch {
    let item = user_types().remove(LANG_STRING).unwrap();
    let Arrow::Struct(fields) = item.clone() else {
        unreachable!()
    };
    let values = StructArray::new(
        fields,
        vec![
            Arc::new(StringArray::from(vec!["cat", "gato"])) as ArrayRef,
            Arc::new(StringArray::from(vec!["en", "es"])),
        ],
        None,
    );
    let labels = ListArray::new(
        Arc::new(Field::new("item", item, false)),
        OffsetBuffer::new(vec![0, 2, 2, 2].into()),
        Arc::new(values),
        Some(NullBuffer::from(vec![true, true, false])),
    );
    batch(vec![
        ("id", Arc::new(Int64Array::from_iter_values([1, 2, 3]))),
        ("labels", Arc::new(labels)),
    ])
}

/// Writes the `user_defined` graph under `root`, the graph document last.
pub async fn write_user_defined(store: &dyn ObjectStore, root: &Path) -> Result<(), Error> {
    write_vertices(
        store,
        root,
        &concept(),
        &[concept_rows()],
        &Options {
            user_types: user_types(),
            ..Options::default()
        },
    )
    .await?;
    write_info(store, root, &user_defined_graph()).await
}
