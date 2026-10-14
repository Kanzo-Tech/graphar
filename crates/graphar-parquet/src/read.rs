//! The readers: one chunk at a time, as C++'s `VertexPropertyArrowChunkReader`,
//! `AdjListArrowChunkReader`, `AdjListPropertyArrowChunkReader` and
//! `AdjListOffsetArrowChunkReader` read one (`arrow/chunk_reader.cc`). A chunk is read whole, then
//! decoded; what it holds is what the file holds.

use arrow_array::{Array, Int32Array, Int64Array, RecordBatch};
use graphar_info::{AdjacentList, EdgeInfo, FileType, PropertyGroup, VertexInfo};
use object_store::{GetOptions, ObjectStore, path::Path};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;

use crate::error::{Error, invalid};
use crate::write::at;
use crate::{OFFSET_COL, paths};

async fn get(store: &dyn ObjectStore, root: &Path, relative: &str) -> Result<bytes::Bytes, Error> {
    let path = at(root, relative)?;
    Ok(store
        .get_opts(&path, GetOptions::default())
        .await?
        .bytes()
        .await?)
}

async fn get_parquet(
    store: &dyn ObjectStore,
    root: &Path,
    relative: &str,
    at: &str,
    file_type: FileType,
) -> Result<Vec<RecordBatch>, Error> {
    if file_type != FileType::Parquet {
        return Err(Error::FileType {
            at: at.to_owned(),
            file_type,
        });
    }
    let bytes = get(store, root, relative).await?;
    let reader = ParquetRecordBatchReaderBuilder::try_new(bytes)?.build()?;
    Ok(reader.collect::<Result<_, _>>()?)
}

/// **Reads a count file** — `vertex_count`, `edge_count<p>`, or an adjacency list's
/// `vertex_count`, at a path from [`paths`]: one `int64`, little-endian.
pub async fn read_count(store: &dyn ObjectStore, root: &Path, path: &str) -> Result<u64, Error> {
    let bytes = get(store, root, path).await?;
    let raw: [u8; 8] = bytes[..]
        .try_into()
        .map_err(|_| invalid(path, format!("{} bytes, where a count is 8", bytes.len())))?;
    u64::try_from(i64::from_le_bytes(raw)).map_err(|_| invalid(path, "a negative count"))
}

/// **Reads chunk `chunk` of a vertex property group**: `_graphArVertexIndex`, then the group's
/// properties.
pub async fn read_vertex_chunk(
    store: &dyn ObjectStore,
    root: &Path,
    info: &VertexInfo,
    group: &PropertyGroup,
    chunk: u64,
) -> Result<Vec<RecordBatch>, Error> {
    let path = paths::vertex_chunk(info, group, chunk);
    let at = format!("vertex `{}`, {path}", info.r#type);
    get_parquet(store, root, &path, &at, group.file_type).await
}

/// **Reads chunk `chunk` of part `part` of an adjacency list**: `_graphArSrcIndex` and
/// `_graphArDstIndex`.
pub async fn read_adj_list_chunk(
    store: &dyn ObjectStore,
    root: &Path,
    info: &EdgeInfo,
    adj: &AdjacentList,
    part: u64,
    chunk: u64,
) -> Result<Vec<RecordBatch>, Error> {
    let path = paths::adj_list_chunk(info, adj, part, chunk);
    let at = format!("edge `{}`, {path}", info.triple());
    get_parquet(store, root, &path, &at, adj.file_type).await
}

/// **Reads chunk `chunk` of part `part` of an edge property group**: the group's properties, row
/// for row beside the same chunk of the adjacency list.
pub async fn read_edge_property_chunk(
    store: &dyn ObjectStore,
    root: &Path,
    info: &EdgeInfo,
    adj: &AdjacentList,
    group: &PropertyGroup,
    part: u64,
    chunk: u64,
) -> Result<Vec<RecordBatch>, Error> {
    let path = paths::edge_property_chunk(info, adj, group, part, chunk);
    let at = format!("edge `{}`, {path}", info.triple());
    get_parquet(store, root, &path, &at, group.file_type).await
}

/// **Reads the offset chunk of part `part`** of an ordered adjacency list: for each vertex of the
/// vertex chunk, where its edges begin in the part, and one more entry, where the last one's end.
/// The specification does not give the column's type. `_graphArOffset` is `int64` as C++ writes it, or `int32` as GraphAr's Spark writer does
/// (`EdgeWriter.scala`); either is answered as `i64`.
pub async fn read_offsets(
    store: &dyn ObjectStore,
    root: &Path,
    info: &EdgeInfo,
    adj: &AdjacentList,
    part: u64,
) -> Result<Vec<i64>, Error> {
    let path = paths::offset_chunk(info, adj, part);
    let at = format!("edge `{}`, {path}", info.triple());
    let mut offsets = Vec::new();
    for batch in get_parquet(store, root, &path, &at, adj.file_type).await? {
        let column = batch
            .column_by_name(OFFSET_COL)
            .ok_or_else(|| invalid(&at, format!("no column `{OFFSET_COL}`")))?;
        if column.null_count() > 0 {
            return Err(invalid(&at, "an offset is null"));
        }
        if let Some(c) = column.as_any().downcast_ref::<Int64Array>() {
            offsets.extend(c.values().iter().copied());
        } else if let Some(c) = column.as_any().downcast_ref::<Int32Array>() {
            offsets.extend(c.values().iter().map(|&o| i64::from(o)));
        } else {
            return Err(invalid(
                &at,
                format!("`{OFFSET_COL}` is {}", column.data_type()),
            ));
        }
    }
    Ok(offsets)
}
