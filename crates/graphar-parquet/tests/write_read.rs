//! The sample graph written to memory and read back, file by file, through the paths GraphAr's
//! C++ library composes.

mod sample;

use std::sync::Arc;

use arrow_array::{Array, Float64Array, Int32Array, Int64Array, RecordBatch, StringArray};
use arrow_schema::DataType as Arrow;
use futures::executor::block_on;
use graphar_info::{AdjListType, DataType, FileType, GraphInfo};
use graphar_parquet::{
    DST_INDEX_COL, Error, Options, SRC_INDEX_COL, VERTEX_INDEX_COL, paths, read_adj_list_chunk,
    read_count, read_edge_property_chunk, read_offsets, read_vertex_chunk, write_edge_vertex_count,
    write_edges, write_vertex_count, write_vertices,
};
use object_store::{ObjectStore, ObjectStoreExt, memory::InMemory, path::Path};

fn written() -> (InMemory, Path) {
    let store = InMemory::new();
    let root = Path::from("graphs/sample");
    block_on(sample::write(&store, &root)).expect("the sample writes");
    (store, root)
}

fn ints(batches: &[RecordBatch], name: &str) -> Vec<i64> {
    batches
        .iter()
        .flat_map(|b| {
            let c = b
                .column_by_name(name)
                .unwrap_or_else(|| panic!("no `{name}`"));
            c.as_any()
                .downcast_ref::<Int64Array>()
                .expect("int64")
                .values()
                .to_vec()
        })
        .collect()
}

fn names(batches: &[RecordBatch]) -> Vec<String> {
    batches[0]
        .schema()
        .fields()
        .iter()
        .map(|f| f.name().clone())
        .collect()
}

#[test]
fn every_file_is_where_cpp_looks() {
    let (store, root) = written();
    let mut listed: Vec<String> = block_on(async {
        use futures::StreamExt;
        store
            .list(Some(&root))
            .map(|m| m.unwrap().location.to_string())
            .collect()
            .await
    });
    listed.sort();
    let under = |p: &str| format!("graphs/sample/{p}");
    for expected in [
        "sample.graph.yml",
        "person.vertex.yml",
        "person_knows_person.edge.yml",
        "vertex/person/vertex_count",
        "vertex/person/id/chunk0",
        "vertex/person/id/chunk2",
        "vertex/person/name_age_born_active/chunk2",
        "edge/person_knows_person/ordered_by_source/vertex_count",
        "edge/person_knows_person/ordered_by_source/edge_count1",
        "edge/person_knows_person/ordered_by_source/offset/chunk1",
        "edge/person_knows_person/ordered_by_source/adj_list/part0/chunk1",
        "edge/person_knows_person/ordered_by_source/weight/part2/chunk0",
        "edge/person_knows_person/unordered_by_dest/adj_list/part1/chunk0",
    ] {
        assert!(
            listed.contains(&under(expected)),
            "{expected} missing from {listed:#?}"
        );
    }
    // An empty part has its count and offsets, and no chunk; an unordered list has no offsets.
    for absent in [
        "edge/person_knows_person/ordered_by_source/adj_list/part1/chunk0",
        "edge/person_knows_person/unordered_by_dest/offset/chunk0",
        "vertex/person/id/chunk3",
    ] {
        assert!(!listed.contains(&under(absent)), "{absent} written");
    }
}

#[test]
fn vertex_chunks_carry_the_internal_id_then_the_group() {
    let (store, root) = written();
    let person = sample::person();
    assert_eq!(
        block_on(read_count(&store, &root, &paths::vertex_count(&person))).unwrap(),
        7
    );
    let group = &person.property_groups[1];
    let mut ids = Vec::new();
    let mut names_read = Vec::new();
    for chunk in 0..3 {
        let batches = block_on(read_vertex_chunk(&store, &root, &person, group, chunk)).unwrap();
        assert_eq!(
            names(&batches),
            [VERTEX_INDEX_COL, "name", "age", "born", "active"]
        );
        let schema = batches[0].schema();
        assert!(!schema.field(0).is_nullable());
        assert_eq!(schema.field(0).data_type(), &Arrow::Int64);
        assert!(!schema.field(1).is_nullable(), "`name` is not nullable");
        assert!(schema.field(2).is_nullable(), "`age` is nullable");
        ids.extend(ints(&batches, VERTEX_INDEX_COL));
        for b in &batches {
            let n = b.column(1).as_any().downcast_ref::<StringArray>().unwrap();
            names_read.extend(n.iter().map(|s| s.unwrap().to_owned()));
        }
        if chunk == 0 {
            let age = batches[0]
                .column(2)
                .as_any()
                .downcast_ref::<Int32Array>()
                .unwrap();
            assert_eq!(age.iter().collect::<Vec<_>>(), [Some(31), Some(25), None]);
        }
    }
    assert_eq!(ids, (0..7).collect::<Vec<_>>());
    assert_eq!(names_read, sample::PERSONS);
}

#[test]
fn a_chunk_is_one_row_group_that_counts_its_distinct_values() {
    use parquet::file::reader::{FileReader, SerializedFileReader};

    let (store, root) = written();
    let person = sample::person();
    let chunk = paths::vertex_chunk(&person, &person.property_groups[1], 0);
    let path = Path::parse(format!("{root}/{chunk}")).unwrap();
    let bytes = block_on(async { store.get(&path).await?.bytes().await }).unwrap();
    let footer = SerializedFileReader::new(bytes).unwrap();
    let metadata = footer.metadata();
    assert_eq!(metadata.num_row_groups(), 1);
    let distinct = |name: &str| {
        let group = metadata.row_group(0);
        let column = (0..group.num_columns())
            .map(|c| group.column(c))
            .find(|c| c.column_descr().name() == name)
            .unwrap();
        let stats = column.statistics().unwrap();
        (stats.distinct_count_opt(), stats.null_count_opt())
    };
    // Chunk 0's ages are 31, 25 and a null: two values and one null, as `pg_stats` counts them.
    assert_eq!(distinct("age"), (Some(2), Some(1)));
    assert_eq!(distinct(VERTEX_INDEX_COL), (Some(3), Some(0)));
}

#[test]
fn an_ordered_list_is_partitioned_sorted_and_offset() {
    let (store, root) = written();
    let knows = sample::knows();
    let adj = &knows.adj_lists[0];
    assert_eq!(adj.r#type, AdjListType::OrderedBySource);
    let count = |p: &str| block_on(read_count(&store, &root, p)).unwrap();
    assert_eq!(count(&paths::edge_vertex_count(&knows, adj)), 7);
    let per_part: Vec<u64> = (0..3)
        .map(|p| count(&paths::edge_count(&knows, adj, p)))
        .collect();
    assert_eq!(per_part, [6, 0, 2]);

    let offsets: Vec<Vec<i64>> = (0..3)
        .map(|p| block_on(read_offsets(&store, &root, &knows, adj, p)).unwrap())
        .collect();
    // Part 0: persons 0, 1, 2 have 3, 2 and 1 edges. Part 2: person 6 has 2, and is the last
    // vertex, so the chunk is 2 rows long, aligned with the vertex chunk.
    assert_eq!(offsets, [vec![0, 3, 5, 6], vec![0, 0, 0, 0], vec![0, 2]]);

    let mut edges = Vec::new();
    let mut weights = Vec::new();
    for (part, chunks) in [(0, 2), (2, 1)] {
        for chunk in 0..chunks {
            let adj_list =
                block_on(read_adj_list_chunk(&store, &root, &knows, adj, part, chunk)).unwrap();
            assert_eq!(names(&adj_list), [SRC_INDEX_COL, DST_INDEX_COL]);
            let (s, d) = (
                ints(&adj_list, SRC_INDEX_COL),
                ints(&adj_list, DST_INDEX_COL),
            );
            assert!(s.len() <= 4, "chunk_size is 4");
            edges.extend(s.into_iter().zip(d));
            let group = &knows.property_groups[0];
            let props = block_on(read_edge_property_chunk(
                &store, &root, &knows, adj, group, part, chunk,
            ))
            .unwrap();
            assert_eq!(names(&props), ["weight"]);
            for b in &props {
                let w = b.column(0).as_any().downcast_ref::<Float64Array>().unwrap();
                weights.extend(w.values().iter().copied());
            }
        }
    }
    // Sorted by source, and stable: edges of one source keep the order they were handed in.
    assert_eq!(
        edges,
        [
            (0, 1),
            (0, 2),
            (0, 6),
            (1, 2),
            (1, 0),
            (2, 6),
            (6, 5),
            (6, 0)
        ]
    );
    let weight_of = |e: (i64, i64)| sample::KNOWS.iter().find(|k| (k.0, k.1) == e).unwrap().2;
    assert_eq!(
        weights,
        edges.iter().map(|&e| weight_of(e)).collect::<Vec<_>>()
    );
}

#[test]
fn an_unordered_list_is_partitioned_and_keeps_its_order() {
    let (store, root) = written();
    let knows = sample::knows();
    let adj = &knows.adj_lists[1];
    assert_eq!(adj.r#type, AdjListType::UnorderedByDest);
    let mut parts = Vec::new();
    for part in 0..3 {
        let n = block_on(read_count(
            &store,
            &root,
            &paths::edge_count(&knows, adj, part),
        ))
        .unwrap();
        let mut edges = Vec::new();
        for chunk in 0..n.div_ceil(4) {
            let b = block_on(read_adj_list_chunk(&store, &root, &knows, adj, part, chunk)).unwrap();
            edges.extend(
                ints(&b, SRC_INDEX_COL)
                    .into_iter()
                    .zip(ints(&b, DST_INDEX_COL)),
            );
        }
        parts.push(edges);
    }
    assert_eq!(
        parts,
        [
            vec![(0, 1), (0, 2), (1, 2), (6, 0), (1, 0)],
            vec![(6, 5)],
            vec![(2, 6), (0, 6)],
        ]
    );
}

#[test]
fn the_information_files_load_back() {
    let (store, root) = written();
    let read = |name: &str| {
        let path = root
            .parts()
            .chain(Path::from(name).parts())
            .collect::<Path>();
        let bytes = block_on(async { store.get(&path).await?.bytes().await }).unwrap();
        Ok::<_, graphar_info::Error>(String::from_utf8(bytes.to_vec()).unwrap())
    };
    let graph = GraphInfo::load(&read("sample.graph.yml").unwrap(), read).unwrap();
    assert_eq!(graph, sample::graph());
}

/// `rows` with column `name` replaced by `array`.
/// Every file under `root`, by path, with its bytes.
fn files(store: &InMemory, root: &Path) -> Vec<(String, Vec<u8>)> {
    let mut files: Vec<(String, Vec<u8>)> = block_on(async {
        use futures::{StreamExt, TryStreamExt};
        let metas: Vec<_> = store.list(Some(root)).try_collect().await.unwrap();
        futures::stream::iter(metas)
            .then(|m| async move {
                let bytes = store.get(&m.location).await.unwrap().bytes().await.unwrap();
                (m.location.to_string(), bytes.to_vec())
            })
            .collect()
            .await
    });
    files.sort();
    files
}

#[test]
fn a_table_written_chunk_by_chunk_is_the_table_written_whole() {
    let person = sample::person();
    let knows = sample::knows();
    let adj = AdjListType::OrderedBySource;
    let rows = sample::person_rows();
    let edges = sample::knows_rows();
    let options = Options::default();
    let whole = |store: &InMemory, root: &Path| {
        block_on(async {
            let n = write_vertices(
                store,
                root,
                &person,
                0,
                std::slice::from_ref(&rows),
                &options,
            )
            .await?;
            write_vertex_count(store, root, &person, n).await?;
            write_edges(
                store,
                root,
                &knows,
                adj,
                0..n,
                std::slice::from_ref(&edges),
                &options,
            )
            .await?;
            write_edge_vertex_count(store, root, &knows, adj, n).await
        })
        .unwrap();
    };
    // One vertex chunk at a time, and the part of the edges from it, as a streaming writer holds
    // them: 7 people in chunks of 3, so the last chunk and part are one vertex long.
    let by_chunk = |store: &InMemory, root: &Path| {
        block_on(async {
            let n = rows.num_rows() as u64;
            let src = edges
                .column_by_name(SRC_INDEX_COL)
                .unwrap()
                .as_any()
                .downcast_ref::<Int64Array>()
                .unwrap()
                .clone();
            for k in 0..n.div_ceil(person.chunk_size) {
                let begin = k * person.chunk_size;
                let end = (begin + person.chunk_size).min(n);
                let chunk = rows.slice(begin as usize, (end - begin) as usize);
                write_vertices(store, root, &person, k, &[chunk], &options).await?;
                let from: arrow_array::BooleanArray = src
                    .iter()
                    .map(|s| s.map(|s| (begin..end).contains(&(s as u64))))
                    .collect();
                let part = arrow_select::filter::filter_record_batch(&edges, &from).unwrap();
                write_edges(store, root, &knows, adj, begin..end, &[part], &options).await?;
            }
            write_vertex_count(store, root, &person, n).await?;
            write_edge_vertex_count(store, root, &knows, adj, n).await
        })
        .unwrap();
    };
    let (a, b) = (InMemory::new(), InMemory::new());
    let root = Path::from("g");
    whole(&a, &root);
    by_chunk(&b, &root);
    let (a, b) = (files(&a, &root), files(&b, &root));
    assert!(a.iter().any(|(p, _)| p.ends_with("offset/chunk2")));
    assert_eq!(
        a.iter().map(|f| &f.0).collect::<Vec<_>>(),
        b.iter().map(|f| &f.0).collect::<Vec<_>>()
    );
    for ((p, x), (_, y)) in a.iter().zip(&b) {
        assert!(x == y, "{p}: the same bytes");
    }

    // A part that does not start a vertex chunk, or an edge outside the part, is refused.
    let store = InMemory::new();
    let e = refused(block_on(write_edges(
        &store,
        &root,
        &knows,
        adj,
        1..3,
        &[edges.slice(0, 0)],
        &options,
    )));
    assert!(e.contains("do not start a vertex chunk"), "{e}");
    let e = refused(block_on(write_edges(
        &store,
        &root,
        &knows,
        adj,
        3..6,
        &[edges],
        &options,
    )));
    assert!(e.contains("is not one of the vertices 3..6"), "{e}");
}

fn with_column(rows: &RecordBatch, name: &str, array: Arc<dyn Array>) -> RecordBatch {
    let schema = rows.schema();
    let columns = schema.fields().iter().zip(rows.columns()).map(|(f, c)| {
        let c = if f.name() == name {
            array.clone()
        } else {
            c.clone()
        };
        (f.name().clone(), c)
    });
    RecordBatch::try_from_iter(columns).unwrap()
}

fn refused(result: Result<u64, Error>) -> String {
    match result {
        Err(e) => e.to_string(),
        Ok(n) => panic!("written: {n}"),
    }
}

#[test]
fn a_column_that_does_not_match_the_information_file_is_refused() {
    let store = InMemory::new();
    let root = Path::from("g");

    let person = sample::person();
    let rows = sample::person_rows();

    // `age` is `int32`; an `int64` column is not it.
    let wrong = with_column(&rows, "age", Arc::new(Int64Array::from(vec![1; 7])));
    let e = refused(block_on(write_vertices(
        &store,
        &root,
        &person,
        0,
        &[wrong],
        &Options::default(),
    )));
    assert!(e.contains("property `age`") && e.contains("int32"), "{e}");

    // `name` is not nullable.
    let names = StringArray::from(vec![
        Some("a"),
        None,
        Some("c"),
        Some("d"),
        Some("e"),
        Some("f"),
        Some("g"),
    ]);
    let with_null = with_column(&rows, "name", Arc::new(names));
    let e = refused(block_on(write_vertices(
        &store,
        &root,
        &person,
        0,
        &[with_null],
        &Options::default(),
    )));
    assert!(e.contains("not nullable"), "{e}");

    // A missing property.
    let e = refused(block_on(write_vertices(
        &store,
        &root,
        &person,
        0,
        &[rows.project(&[0]).unwrap()],
        &Options::default(),
    )));
    assert!(e.contains("no column `name`"), "{e}");
}

#[test]
fn an_edge_beyond_the_vertex_count_is_refused() {
    let store = InMemory::new();

    let e = refused(block_on(write_edges(
        &store,
        &Path::from("g"),
        &sample::knows(),
        AdjListType::OrderedBySource,
        0..3,
        &[sample::knows_rows()],
        &Options::default(),
    )));
    assert!(e.contains("is not one of the vertices 0..3"), "{e}");
    let e = refused(block_on(write_edges(
        &store,
        &Path::from("g"),
        &sample::knows(),
        AdjListType::OrderedByDest,
        0..7,
        &[sample::knows_rows()],
        &Options::default(),
    )));
    assert!(e.contains("no such adjacency list"), "{e}");
}

#[test]
fn only_parquet_is_written() {
    let store = InMemory::new();
    let mut person = sample::person();
    person.property_groups[0].file_type = FileType::Csv;
    let e = refused(block_on(write_vertices(
        &store,
        &Path::from("g"),
        &person,
        0,
        &[sample::person_rows()],
        &Options::default(),
    )));
    assert!(e.contains("`csv` is not Parquet"), "{e}");
}

#[test]
fn a_dot_prefix_is_not_a_path() {
    // `./` is what C++ resolves against the working directory; as an object path it is refused.
    let store = InMemory::new();
    let mut person = sample::person();
    person.prefix = "./vertex/person/".to_owned();
    let e = refused(block_on(write_vertices(
        &store,
        &Path::from("g"),
        &person,
        0,
        &[sample::person_rows()],
        &Options::default(),
    )));
    assert!(e.contains("Path \"./vertex/person/id/chunk0\""), "{e}");
}

#[test]
fn a_list_property_is_an_arrow_list() {
    use arrow_array::builder::{ListBuilder, StringBuilder};
    let store = InMemory::new();
    let root = Path::from("g");
    let mut v = sample::nickname();
    v.property_groups[0].properties[1].data_type = DataType::List(Box::new(DataType::String));
    let mut values = ListBuilder::new(StringBuilder::new());
    for vs in [["a", "b"], ["c", "d"], ["e", "f"]] {
        for s in vs {
            values.values().append_value(s);
        }
        values.append(true);
    }
    let rows = RecordBatch::try_from_iter([
        (
            "src",
            Arc::new(Int64Array::from(vec![0, 1, 2])) as Arc<dyn Array>,
        ),
        ("value", Arc::new(values.finish())),
    ])
    .unwrap();
    let n = block_on(write_vertices(
        &store,
        &root,
        &v,
        0,
        &[rows],
        &Options::default(),
    ))
    .unwrap();
    assert_eq!(n, 3);
    let back = block_on(read_vertex_chunk(
        &store,
        &root,
        &v,
        &v.property_groups[0],
        0,
    ))
    .unwrap();
    assert!(matches!(
        back[0].schema().field(2).data_type(),
        Arrow::List(_)
    ));
}

#[test]
fn a_list_of_a_user_defined_struct_is_a_parquet_list_of_struct() {
    use arrow_array::{ListArray, StructArray};
    use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;

    let store = InMemory::new();
    let root = Path::from("g");
    block_on(sample::write_user_defined(&store, &root)).unwrap();
    let concept = sample::concept();
    let group = &concept.property_groups[0];

    // The file: `labels` is LIST<STRUCT<value: STRING, lang: STRING>> in Parquet's own schema.
    let relative = Path::from(paths::vertex_chunk(&concept, group, 0));
    let path: Path = root.parts().chain(relative.parts()).collect();
    let bytes = block_on(async { store.get(&path).await.unwrap().bytes().await.unwrap() });
    let builder = ParquetRecordBatchReaderBuilder::try_new(bytes).unwrap();
    let leaves: Vec<String> = builder
        .metadata()
        .file_metadata()
        .schema_descr()
        .columns()
        .iter()
        .map(|c| c.path().string())
        .collect();
    assert_eq!(leaves.len(), 4);
    assert_eq!(leaves[0], VERTEX_INDEX_COL.to_owned());
    assert_eq!(leaves[1], "id");
    // Parquet's own LIST nesting, then the struct's two leaves.
    assert!(
        leaves[2].starts_with("labels.") && leaves[2].ends_with(".value"),
        "{leaves:?}"
    );
    assert!(
        leaves[3].starts_with("labels.") && leaves[3].ends_with(".lang"),
        "{leaves:?}"
    );

    // The read back: the values, the empty list and the null list.
    let back = block_on(read_vertex_chunk(&store, &root, &concept, group, 0)).unwrap();
    let labels = back[0].column_by_name("labels").unwrap();
    assert_eq!(
        labels.data_type(),
        sample::concept_rows().column(1).data_type()
    );
    let list = labels.as_any().downcast_ref::<ListArray>().unwrap();
    assert!(list.is_null(2));
    assert_eq!(list.value(1).len(), 0);
    let first = list.value(0);
    let pairs = first.as_any().downcast_ref::<StructArray>().unwrap();
    let text = |name: &str| {
        let c = pairs.column_by_name(name).unwrap();
        let c = c.as_any().downcast_ref::<StringArray>().unwrap();
        c.iter().map(|s| s.unwrap().to_owned()).collect::<Vec<_>>()
    };
    assert_eq!(text("value"), ["cat", "gato"]);
    assert_eq!(text("lang"), ["en", "es"]);
}

#[test]
fn a_user_defined_type_is_held_only_by_the_arrow_type_the_caller_gave() {
    let store = InMemory::new();
    let root = Path::from("g");

    let concept = sample::concept();
    let rows = sample::concept_rows();

    // No Arrow type for the name: nothing can hold it.
    let e = refused(block_on(write_vertices(
        &store,
        &root,
        &concept,
        0,
        std::slice::from_ref(&rows),
        &Options::default(),
    )));
    assert!(
        e.contains("property `labels`") && e.contains("`langString`"),
        "{e}"
    );

    // A different one.
    let mut other = sample::user_types();
    other.insert("langString".to_owned(), Arrow::Utf8);
    let e = refused(block_on(write_vertices(
        &store,
        &root,
        &concept,
        0,
        &[rows],
        &Options {
            user_types: other,
            ..Options::default()
        },
    )));
    assert!(e.contains("property `labels`"), "{e}");
}
