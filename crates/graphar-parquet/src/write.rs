//! The writers: C++ `VertexPropertyWriter` and `EdgeChunkWriter` (`arrow/chunk_writer.cc`), with
//! what C++'s `EdgesBuilder::Dump` adds — a count and an offset chunk for every part, empty or not.

use std::sync::Arc;

use arrow_array::{Array, ArrayRef, Int64Array, RecordBatch, UInt64Array};
use arrow_schema::{DataType as Arrow, Field, Schema, SchemaRef};
use arrow_select::{concat::concat_batches, take::take_record_batch};
use graphar_info::{
    AdjListType, EdgeInfo, FileType, GraphInfo, Property, PropertyGroup, VertexInfo,
};
use object_store::{ObjectStore, PutOptions, PutPayload, path::Path};
use parquet::{arrow::ArrowWriter, file::properties::WriterProperties};

use crate::error::{Error, invalid};
use crate::types::{Options, UserTypes, holds_property};
use crate::{DST_INDEX_COL, OFFSET_COL, SRC_INDEX_COL, VERTEX_INDEX_COL, paths};

/// `root` joined with a path from [`paths`].
pub(crate) fn at(root: &Path, relative: &str) -> Result<Path, Error> {
    Ok(root.parts().chain(Path::parse(relative)?.parts()).collect())
}

async fn put(
    store: &dyn ObjectStore,
    root: &Path,
    relative: &str,
    bytes: Vec<u8>,
) -> Result<(), Error> {
    store
        .put_opts(
            &at(root, relative)?,
            PutPayload::from(bytes),
            PutOptions::default(),
        )
        .await?;
    Ok(())
}

/// A count file: one `int64`, as C++'s `FileSystem::WriteValueToFile<IdType>` writes it — its
/// bytes in memory, which is little-endian on every platform GraphAr builds for.
async fn put_count(
    store: &dyn ObjectStore,
    root: &Path,
    relative: &str,
    count: u64,
) -> Result<(), Error> {
    let count = i64::try_from(count).map_err(|_| invalid(relative, "a count above i64::MAX"))?;
    put(store, root, relative, count.to_le_bytes().to_vec()).await
}

async fn put_parquet(
    store: &dyn ObjectStore,
    root: &Path,
    relative: &str,
    batch: &RecordBatch,
    props: &WriterProperties,
) -> Result<(), Error> {
    let mut writer = ArrowWriter::try_new(Vec::new(), batch.schema(), Some(props.clone()))?;
    writer.write(batch)?;
    put(store, root, relative, writer.into_inner()?).await
}

fn parquet_only(at: impl Into<String>, file_type: FileType) -> Result<(), Error> {
    match file_type {
        FileType::Parquet => Ok(()),
        file_type => Err(Error::FileType {
            at: at.into(),
            file_type,
        }),
    }
}

/// The batches as one, or an empty batch of no columns when there are none.
fn one(at: &str, batches: &[RecordBatch]) -> Result<RecordBatch, Error> {
    match batches.first() {
        None => Ok(RecordBatch::new_empty(Arc::new(Schema::empty()))),
        Some(first) => concat_batches(&first.schema(), batches)
            .map_err(|e| invalid(at, format!("the batches do not share one schema: {e}"))),
    }
}

/// The column `name` of `batch`, which must be there.
fn column<'a>(at: &str, batch: &'a RecordBatch, name: &str) -> Result<&'a ArrayRef, Error> {
    batch
        .column_by_name(name)
        .ok_or_else(|| invalid(at, format!("no column `{name}`")))
}

/// The columns of `group`, in the information file's order and with its nullability, checked
/// against it — the types C++'s writer checks, and the nulls it does not.
fn group_columns(
    at: &str,
    batch: &RecordBatch,
    group: &PropertyGroup,
    user: &UserTypes,
) -> Result<Vec<(Field, ArrayRef)>, Error> {
    group
        .properties
        .iter()
        .map(|p: &Property| {
            let array = column(at, batch, &p.name)?;
            if !holds_property(p, array.data_type(), user) {
                return Err(invalid(
                    format!("{at}, property `{}`", p.name),
                    format!(
                        "`{}` is not held by Arrow {}",
                        p.data_type,
                        array.data_type()
                    ),
                ));
            }
            if !p.is_nullable && array.null_count() > 0 {
                return Err(invalid(
                    format!("{at}, property `{}`", p.name),
                    "it is not nullable, and holds a null",
                ));
            }
            let field = Field::new(&p.name, array.data_type().clone(), p.is_nullable);
            Ok((field, array.clone()))
        })
        .collect()
}

fn batch_of(columns: Vec<(Field, ArrayRef)>) -> Result<RecordBatch, Error> {
    let (fields, arrays): (Vec<Field>, Vec<ArrayRef>) = columns.into_iter().unzip();
    let schema: SchemaRef = Arc::new(Schema::new(fields));
    Ok(RecordBatch::try_new(schema, arrays)?)
}

fn index_field(name: &str) -> Field {
    Field::new(name, Arrow::Int64, false)
}

/// The number of chunks `count` rows make at `chunk_size` rows each.
fn chunks(count: u64, chunk_size: u64) -> u64 {
    count.div_ceil(chunk_size)
}

/// **Writes a vertex type's logical table**: every chunk of every property group, then the type's
/// `vertex_count`, and returns that count.
///
/// The rows of `batches` are the vertices in order, so a vertex's internal id is its row. A batch
/// holds a column per property, by name; other columns are not read. Each chunk of each group is
/// `_graphArVertexIndex` (`int64`, the id) then the group's properties, in the information file's
/// order and nullability. A property's column must hold its GraphAr type, and one that is not
/// nullable must hold no null. A property of a user-defined type (`gar/v1 (langString)`) must be
/// held by the Arrow type `options.user_types` gives its name, which GraphAr does not define.
///
/// It writes no information file: [`write_info`] does, once every table is written.
pub async fn write_vertices(
    store: &dyn ObjectStore,
    root: &Path,
    info: &VertexInfo,
    batches: &[RecordBatch],
    options: &Options,
) -> Result<u64, Error> {
    info.validate()?;
    let at = format!("vertex `{}`", info.r#type);
    for (i, group) in info.property_groups.iter().enumerate() {
        parquet_only(format!("{at}, property group {i}"), group.file_type)?;
    }
    let table = one(&at, batches)?;
    let count = table.num_rows() as u64;
    let mut checked = Vec::with_capacity(info.property_groups.len());
    for group in &info.property_groups {
        checked.push(group_columns(&at, &table, group, &options.user_types)?);
    }
    for k in 0..chunks(count, info.chunk_size) {
        let begin = k * info.chunk_size;
        let len = info.chunk_size.min(count - begin);
        let ids: ArrayRef = Arc::new(Int64Array::from_iter_values(
            (begin..begin + len).map(|i| i as i64),
        ));
        for (group, columns) in info.property_groups.iter().zip(&checked) {
            let mut chunk = vec![(index_field(VERTEX_INDEX_COL), ids.clone())];
            chunk.extend(
                columns.iter().map(|(field, array)| {
                    (field.clone(), array.slice(begin as usize, len as usize))
                }),
            );
            let path = paths::vertex_chunk(info, group, k);
            put_parquet(store, root, &path, &batch_of(chunk)?, &options.props).await?;
        }
    }
    put_count(store, root, &paths::vertex_count(info), count).await?;
    Ok(count)
}

/// **Writes one adjacency list of an edge type**, with its property groups, and returns the
/// number of edges.
///
/// `vertex_count` is the number of vertices the list is aligned by: the source type's for a list
/// by source, the destination type's for one by destination. A batch holds `_graphArSrcIndex` and
/// `_graphArDstIndex` (`int64`, no null: internal ids of the source and destination types) and a
/// column per edge property, by name. `options` is as for [`write_vertices`].
///
/// The edges are partitioned by the vertex chunk of their aligned end, a part per vertex chunk,
/// and each part is cut into chunks of `chunk_size` edges. Within a part an ordered list is sorted
/// by that end, and an unordered one keeps the order it is handed; either sort is stable, so edges
/// handed sorted by `(src, dst)` stay so. For every part — an empty one too, as C++'s
/// `EdgesBuilder::Dump` does — it writes `edge_count<p>` and, for an ordered list, the offset
/// chunk: `_graphArOffset`, `int64`, one row per vertex of the vertex chunk plus one, counted from
/// the part's first edge, the first always 0. Then the list's `vertex_count`.
///
/// **A departure from C++, for the specification.** The last offset chunk is as long as the last
/// vertex chunk, plus one. The specification says: "The partition of the offset table should be in
/// alignment with the partition of the corresponding vertex table. The first row of each offset
/// chunk is always 0" (format.md, "Physical table of edges"); it does not give the length, and
/// `ldbc_sample/parquet` of GraphAr's testing data is written so. C++'s `EdgesBuilder` and Spark's
/// `EdgeWriter` pad it to `chunk_size + 1` rows instead; their readers index it by vertex and read
/// either.
///
/// Where the specification is silent, C++ is followed: the file names (`paths`), the count files
/// (one little-endian `int64`, as C++ writes it in memory on its platforms), and a count and an
/// offset chunk for every part, empty or not (`EdgesBuilder::Dump`).
pub async fn write_edges(
    store: &dyn ObjectStore,
    root: &Path,
    info: &EdgeInfo,
    adj_list_type: AdjListType,
    vertex_count: u64,
    batches: &[RecordBatch],
    options: &Options,
) -> Result<u64, Error> {
    info.validate()?;
    let at = format!("edge `{}`, {adj_list_type}", info.triple());
    let adj = info
        .adj_lists
        .iter()
        .find(|a| a.r#type == adj_list_type)
        .ok_or_else(|| invalid(&at, "the edge type has no such adjacency list"))?;
    parquet_only(&at, adj.file_type)?;
    for (i, group) in info.property_groups.iter().enumerate() {
        parquet_only(format!("{at}, property group {i}"), group.file_type)?;
    }
    let table = one(&at, batches)?;
    let ends = |name: &str| -> Result<Int64Array, Error> {
        let array = column(&at, &table, name)?;
        let ids = array
            .as_any()
            .downcast_ref::<Int64Array>()
            .ok_or_else(|| invalid(&at, format!("`{name}` is not int64")))?;
        if ids.null_count() > 0 {
            return Err(invalid(&at, format!("`{name}` holds a null")));
        }
        if ids.values().iter().any(|&id| id < 0) {
            return Err(invalid(&at, format!("`{name}` holds a negative id")));
        }
        Ok(ids.clone())
    };
    let (src, dst) = (ends(SRC_INDEX_COL)?, ends(DST_INDEX_COL)?);
    let (key, vertex_chunk_size) = if adj_list_type.by_source() {
        (&src, info.src_chunk_size)
    } else {
        (&dst, info.dst_chunk_size)
    };
    if let Some(&beyond) = key.values().iter().find(|&&id| id as u64 >= vertex_count) {
        return Err(invalid(
            &at,
            format!("id {beyond} is not below the vertex count, {vertex_count}"),
        ));
    }
    let part_of = |row: usize| key.value(row) as u64 / vertex_chunk_size;

    // A stable sort, by the aligned end for an ordered list and by its part for an unordered one.
    let mut rows: Vec<u64> = (0..table.num_rows() as u64).collect();
    if adj_list_type.ordered() {
        rows.sort_by_key(|&r| key.value(r as usize));
    } else {
        rows.sort_by_key(|&r| part_of(r as usize));
    }
    let order = UInt64Array::from(rows);
    let mut columns: Vec<(Field, ArrayRef)> = vec![
        (index_field(SRC_INDEX_COL), Arc::new(src.clone())),
        (index_field(DST_INDEX_COL), Arc::new(dst.clone())),
    ];
    let mut grouped = Vec::with_capacity(info.property_groups.len());
    for group in &info.property_groups {
        let checked = group_columns(&at, &table, group, &options.user_types)?;
        grouped.push(checked.len());
        columns.extend(checked);
    }
    let sorted = take_record_batch(&batch_of(columns)?, &order)?;
    let key = sorted.column(usize::from(!adj_list_type.by_source()));
    let key = key
        .as_any()
        .downcast_ref::<Int64Array>()
        .expect("an id column");

    let edges = sorted.num_rows() as u64;
    let mut begin = 0u64;
    for part in 0..chunks(vertex_count, vertex_chunk_size) {
        let first_vertex = part * vertex_chunk_size;
        let end = begin
            + key.values()[begin as usize..]
                .iter()
                .take_while(|&&id| (id as u64) / vertex_chunk_size == part)
                .count() as u64;
        put_count(
            store,
            root,
            &paths::edge_count(info, adj, part),
            end - begin,
        )
        .await?;
        if adj_list_type.ordered() {
            // C++ `EdgesBuilder::getOffsetTable`: for each vertex of the chunk, the number of the
            // part's edges whose end is at or before it.
            let ids = &key.values()[begin as usize..end as usize];
            let last_vertex = (first_vertex + vertex_chunk_size).min(vertex_count);
            let mut seen = 0usize;
            let offsets = Int64Array::from_iter_values(std::iter::once(0).chain(
                (first_vertex..last_vertex).map(|v| {
                    seen += ids[seen..].iter().take_while(|&&id| id as u64 <= v).count();
                    seen as i64
                }),
            ));
            let batch = batch_of(vec![(index_field(OFFSET_COL), Arc::new(offsets))])?;
            put_parquet(
                store,
                root,
                &paths::offset_chunk(info, adj, part),
                &batch,
                &options.props,
            )
            .await?;
        }
        for chunk in 0..chunks(end - begin, info.chunk_size) {
            let from = begin + chunk * info.chunk_size;
            let rows = sorted.slice(from as usize, info.chunk_size.min(end - from) as usize);
            let adj_list = rows.project(&[0, 1])?;
            put_parquet(
                store,
                root,
                &paths::adj_list_chunk(info, adj, part, chunk),
                &adj_list,
                &options.props,
            )
            .await?;
            let mut column = 2;
            for (group, &width) in info.property_groups.iter().zip(&grouped) {
                let projection: Vec<usize> = (column..column + width).collect();
                column += width;
                let path = paths::edge_property_chunk(info, adj, group, part, chunk);
                put_parquet(
                    store,
                    root,
                    &path,
                    &rows.project(&projection)?,
                    &options.props,
                )
                .await?;
            }
        }
        begin = end;
    }
    debug_assert_eq!(begin, edges);
    put_count(
        store,
        root,
        &paths::edge_vertex_count(info, adj),
        vertex_count,
    )
    .await?;
    Ok(edges)
}

/// **Commits a graph**: writes every information file under `root` — here the directory of the
/// graph document, which is also the data's root when the graph names no `prefix` — each vertex
/// and edge document first and the graph document last, under the names [`GraphInfo::dump`]
/// gives them. A reader that opens the
/// graph document therefore finds every document it names, and every table, once the writers have
/// returned. That order is this crate's convention: GraphAr defines no commit.
pub async fn write_info(
    store: &dyn ObjectStore,
    root: &Path,
    graph: &GraphInfo,
) -> Result<(), Error> {
    // `dump` validates, and puts the graph document last.
    for (name, text) in graph.dump()? {
        put(store, root, &name, text.into_bytes()).await?;
    }
    Ok(())
}
