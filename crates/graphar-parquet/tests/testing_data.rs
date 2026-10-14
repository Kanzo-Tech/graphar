//! GraphAr's own Parquet test data, read through this crate: `ldbc_sample/parquet` of
//! `apache/incubator-graphar-testing` at `8286724` (the commit `graphar-info`'s fixtures are from),
//! whose data files are not vendored. CI checks it out and names it in `GRAPHAR_TESTING`:
//!
//! ```sh
//! GRAPHAR_TESTING=path/to/incubator-graphar-testing cargo test -p graphar-parquet -- --ignored
//! ```
//!
//! For each graph of the directory, every chunk a count file implies is read, and holds what
//! GraphAr puts there: the internal ids in order and gapless, one adjacency-list row per edge the
//! part counts, a property chunk row for row beside it, and offsets that rise from 0 to the part's
//! count.

use arrow_array::{Array, Int64Array, RecordBatch};
use futures::executor::block_on;
use graphar_info::GraphInfo;
use graphar_parquet::{
    DST_INDEX_COL, SRC_INDEX_COL, VERTEX_INDEX_COL, paths, read_adj_list_chunk, read_count,
    read_edge_property_chunk, read_offsets, read_vertex_chunk,
};
use object_store::{local::LocalFileSystem, path::Path};

fn rows(batches: &[RecordBatch]) -> usize {
    batches.iter().map(RecordBatch::num_rows).sum()
}

fn ids(batches: &[RecordBatch], name: &str) -> Vec<i64> {
    batches
        .iter()
        .flat_map(|b| {
            let c = b
                .column_by_name(name)
                .unwrap_or_else(|| panic!("no `{name}`"));
            assert_eq!(c.null_count(), 0, "`{name}` holds a null");
            c.as_any()
                .downcast_ref::<Int64Array>()
                .expect("int64")
                .values()
                .to_vec()
        })
        .collect()
}

fn read_graph(dir: &std::path::Path, file: &str) {
    let graph = GraphInfo::load_file(dir.join(file)).unwrap();
    graph.validate().unwrap();
    assert_eq!(
        graph.prefix, None,
        "{file}: the data is under the document's directory"
    );
    let store = LocalFileSystem::new_with_prefix(dir).unwrap();
    let root = Path::default();
    let count = |p: &str| block_on(read_count(&store, &root, p)).unwrap();

    for v in &graph.vertices {
        let n = count(&paths::vertex_count(v));
        assert!(n > 0, "{}: no vertices", v.r#type);
        for group in &v.property_groups {
            let mut all = Vec::new();
            for chunk in 0..n.div_ceil(v.chunk_size) {
                let b = block_on(read_vertex_chunk(&store, &root, v, group, chunk)).unwrap();
                all.extend(ids(&b, VERTEX_INDEX_COL));
                for p in &group.properties {
                    assert!(
                        b[0].column_by_name(&p.name).is_some(),
                        "{}: no `{}`",
                        v.r#type,
                        p.name
                    );
                }
            }
            assert_eq!(all, (0..n as i64).collect::<Vec<_>>(), "{}: ids", v.r#type);
        }
    }
    for e in &graph.edges {
        for adj in &e.adj_lists {
            let at = format!("{} {}", e.triple(), adj.r#type);
            let (vertex_chunk_size, aligned) = if adj.r#type.by_source() {
                (e.src_chunk_size, SRC_INDEX_COL)
            } else {
                (e.dst_chunk_size, DST_INDEX_COL)
            };
            let vertices = count(&paths::edge_vertex_count(e, adj));
            for part in 0..vertices.div_ceil(vertex_chunk_size) {
                let edges = count(&paths::edge_count(e, adj, part));
                let mut keys = Vec::new();
                for chunk in 0..edges.div_ceil(e.chunk_size) {
                    let b =
                        block_on(read_adj_list_chunk(&store, &root, e, adj, part, chunk)).unwrap();
                    keys.extend(ids(&b, aligned));
                    for group in &e.property_groups {
                        let p = block_on(read_edge_property_chunk(
                            &store, &root, e, adj, group, part, chunk,
                        ))
                        .unwrap();
                        assert_eq!(rows(&p), rows(&b), "{at}: part {part} chunk {chunk}");
                    }
                }
                assert_eq!(keys.len() as u64, edges, "{at}: part {part}");
                assert!(
                    keys.iter().all(|&k| k as u64 / vertex_chunk_size == part),
                    "{at}: part {part} holds an edge of another part"
                );
                if adj.r#type.ordered() {
                    assert!(keys.is_sorted(), "{at}: part {part} is not ordered");
                    let offsets = block_on(read_offsets(&store, &root, e, adj, part)).unwrap();
                    let in_chunk = vertex_chunk_size.min(vertices - part * vertex_chunk_size);
                    assert_eq!(offsets.len() as u64, in_chunk + 1, "{at}: part {part}");
                    assert_eq!(offsets[0], 0);
                    assert!(offsets.is_sorted(), "{at}: offsets fall");
                    assert_eq!(*offsets.last().unwrap() as u64, edges, "{at}: part {part}");
                }
            }
        }
    }
}

#[test]
#[ignore = "needs incubator-graphar-testing in GRAPHAR_TESTING"]
fn ldbc_sample_parquet_reads() {
    let testing = std::env::var_os("GRAPHAR_TESTING").expect("GRAPHAR_TESTING names the checkout");
    let dir = std::path::Path::new(&testing).join("ldbc_sample/parquet");
    for file in [
        "ldbc_sample.graph.yml",
        "ldbc_sample_date.graph.yml",
        "ldbc_sample_timestamp.graph.yml",
        "ldbc_sample_with_feature.graph.yml",
    ] {
        read_graph(&dir, file);
    }
}
