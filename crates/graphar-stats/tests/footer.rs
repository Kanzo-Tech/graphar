//! A vertex table written with the counts in each chunk's footer, and its Puffin file.

use std::sync::Arc;

use arrow_array::types::Int64Type;
use arrow_array::{ArrayRef, Int64Array, ListArray, RecordBatch, StringArray};
use futures::executor::block_on;
use graphar_info::{
    Cardinality, DataType, FileType, InfoVersion, Property, PropertyGroup, VertexInfo,
};
use graphar_parquet::{Options, paths, write_vertices};
use graphar_stats::{ColumnStats, FOOTER_KEY, Stats, puffin, puffin_path, theta};
use object_store::{ObjectStoreExt, memory::InMemory, path::Path};
use parquet::file::reader::{FileReader, SerializedFileReader};

fn property(name: &str, data_type: DataType, cardinality: Cardinality) -> Property {
    Property {
        name: name.to_owned(),
        data_type,
        is_primary: false,
        is_nullable: true,
        cardinality,
    }
}

fn info() -> VertexInfo {
    VertexInfo {
        r#type: "Person".to_owned(),
        chunk_size: 4,
        prefix: "vertex/Person/".to_owned(),
        labels: Vec::new(),
        property_groups: vec![PropertyGroup {
            properties: vec![
                property("subject", DataType::String, Cardinality::Single),
                property("city", DataType::String, Cardinality::Single),
                property("age", DataType::Int64, Cardinality::Single),
                property("nick", DataType::Int64, Cardinality::Set),
            ],
            file_type: FileType::Parquet,
            prefix: "p/".to_owned(),
        }],
        version: InfoVersion::default(),
    }
}

fn table() -> RecordBatch {
    let subject: ArrayRef = Arc::new(StringArray::from_iter_values(
        (0..6).map(|i| format!("https://example.org/{i}")),
    ));
    let city: ArrayRef = Arc::new(StringArray::from(vec![
        Some("Madrid"),
        Some("Madrid"),
        Some("Oviedo"),
        None,
        Some("Oviedo"),
        Some("Oviedo"),
    ]));
    let age: ArrayRef = Arc::new(Int64Array::from(vec![30, 30, 30, 41, 41, 52]));
    let nick: ArrayRef = Arc::new(ListArray::from_iter_primitive::<Int64Type, _, _>(vec![
        Some(vec![Some(1), Some(2)]),
        Some(vec![Some(1)]),
        Some(vec![]),
        None,
        Some(vec![Some(3)]),
        Some(vec![Some(3), Some(4)]),
    ]));
    RecordBatch::try_from_iter([
        ("subject", subject),
        ("city", city),
        ("age", age),
        ("nick", nick),
    ])
    .expect("a batch")
}

fn stats(distinct_count: u64, common: &[(&str, f64)]) -> ColumnStats {
    ColumnStats {
        distinct_count,
        most_common_vals: common.iter().map(|(v, _)| (*v).to_owned()).collect(),
        most_common_freqs: common.iter().map(|(_, f)| *f).collect(),
    }
}

#[test]
fn each_chunk_carries_its_own_counts_and_the_puffin_file_the_tables() {
    let store = InMemory::new();
    let root = Path::from("g");
    let info = info();
    let options = Options {
        footer: Some(graphar_stats::footer()),
        ..Options::default()
    };
    block_on(write_vertices(&store, &root, &info, &[table()], &options)).expect("written");

    let footer = |chunk: u64| -> Stats {
        let path = format!(
            "g/{}",
            paths::vertex_chunk(&info, &info.property_groups[0], chunk)
        );
        let bytes = block_on(async { store.get(&Path::from(path)).await?.bytes().await })
            .expect("the chunk");
        let reader = SerializedFileReader::new(bytes).expect("parquet");
        let kv = reader
            .metadata()
            .file_metadata()
            .key_value_metadata()
            .expect("metadata");
        let json = kv
            .iter()
            .find(|kv| kv.key == FOOTER_KEY)
            .and_then(|kv| kv.value.clone());
        serde_json::from_str(&json.expect("the key")).expect("the document")
    };

    // Rows 0..4: the null is one of the four rows a frequency is a share of.
    let first = footer(0);
    assert_eq!(first.version, 1);
    let columns: std::collections::BTreeMap<_, _> = first.columns.into_iter().collect();
    assert_eq!(columns["subject"], stats(4, &[]));
    assert_eq!(columns["city"], stats(2, &[("Madrid", 0.5)]));
    assert_eq!(columns["age"], stats(2, &[("30", 0.75)]));
    // A set is counted over its three elements.
    assert_eq!(columns["nick"], stats(2, &[("1", 2.0 / 3.0)]));

    // Rows 4..6.
    let second: std::collections::BTreeMap<_, _> = footer(1).columns.into_iter().collect();
    assert_eq!(second["city"], stats(1, &[("Oviedo", 1.0)]));

    // Over the whole table a distinct count is not the sum of the chunks': `age` is 2 and 2, and
    // 3 in all.
    let puffin_bytes = graphar_stats::puffin(&info, &table()).expect("the sketches");
    let blobs = puffin::read(&puffin_bytes).expect("a Puffin file");
    let ndv: Vec<(i32, &str, Option<f64>)> = blobs
        .iter()
        .map(|(m, data)| {
            (
                m.fields[0],
                m.properties["ndv"].as_str(),
                theta::estimate_serialized(data),
            )
        })
        .collect();
    assert_eq!(
        ndv,
        vec![
            (1, "6", Some(6.0)),
            (2, "2", Some(2.0)),
            (3, "3", Some(3.0)),
            (4, "4", Some(4.0)),
        ]
    );
    assert!(
        blobs
            .iter()
            .all(|(m, _)| m.r#type == puffin::THETA && m.snapshot_id == -1)
    );
    assert_eq!(puffin_path(&info), "vertex/Person/stats.puffin");
}
