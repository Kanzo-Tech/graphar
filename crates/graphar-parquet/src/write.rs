//! The writers: C++ `VertexPropertyWriter` and `EdgeChunkWriter` (`arrow/chunk_writer.cc`), with
//! what C++'s `EdgesBuilder::Dump` adds — a count and an offset chunk for every part, empty or not.

use std::sync::Arc;

use arrow_array::{Array, ArrayRef, Int64Array, RecordBatch, UInt64Array};
use arrow_schema::{DataType as Arrow, Field, Schema, SchemaRef};
use arrow_select::{concat::concat_batches, take::take_record_batch};
use graphar_info::{
    AdjListType, AdjacentList, EdgeInfo, FileType, GraphInfo, Property, PropertyGroup, VertexInfo,
};
use object_store::{ObjectStore, PutOptions, PutPayload, path::Path};
use parquet::arrow::ArrowWriter;
use parquet::file::properties::WriterProperties;

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
    let mut writer = ArrowWriter::try_new(Vec::new(), batch.schema(), Some(counted(props, batch)))?;
    writer.write(batch)?;
    put(store, root, relative, writer.into_inner()?).await
}

/// The caller's settings, with a chunk written as one row group whose `Statistics` carry
/// `distinct_count` beside the nulls and bounds Parquet always counts. Parquet's statistics are per
/// row group, so one group per chunk makes them the chunk's, and a reader combines chunks into a
/// table's without opening a page.
fn counted(props: &WriterProperties, batch: &RecordBatch) -> WriterProperties {
    props
        .clone()
        .into_builder()
        .set_max_row_group_row_count(Some(batch.num_rows().max(1)))
        .set_max_row_group_bytes(None)
        .set_write_row_group_number_distinct_values(true)
        .build()
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
/// It writes no information file: [`write_info`] does, once every table is written. It is
/// [`VertexWriter`] handed every batch at once.
pub async fn write_vertices(
    store: &dyn ObjectStore,
    root: &Path,
    info: &VertexInfo,
    batches: &[RecordBatch],
    options: &Options,
) -> Result<u64, Error> {
    let mut writer = VertexWriter::new(store, root, info, options)?;
    for batch in batches {
        writer.write(batch).await?;
    }
    writer.finish().await
}

/// **Writes a vertex type's logical table from a stream of batches**, a chunk as soon as it is
/// full, so it holds at most one chunk of rows. The rows are the vertices in order, across every
/// call: the first row of a batch is the vertex after the last row of the batch before. What a
/// batch holds is as for [`write_vertices`]. [`finish`](Self::finish) writes the last chunk and
/// the `vertex_count`.
pub struct VertexWriter<'a> {
    store: &'a dyn ObjectStore,
    root: Path,
    info: &'a VertexInfo,
    options: &'a Options,
    at: String,
    /// Rows handed so far.
    count: u64,
    /// The next chunk to write.
    chunk: u64,
    /// Rows of the chunk being filled, as the groups' columns side by side.
    pending: Vec<RecordBatch>,
    pending_rows: u64,
}

impl<'a> VertexWriter<'a> {
    /// A writer for `info`'s table under `root`; nothing is written until a chunk is full.
    ///
    /// # Errors
    ///
    /// An invalid `info`, or a property group in a file type other than Parquet.
    pub fn new(
        store: &'a dyn ObjectStore,
        root: &Path,
        info: &'a VertexInfo,
        options: &'a Options,
    ) -> Result<Self, Error> {
        info.validate()?;
        let at = format!("vertex `{}`", info.r#type);
        for (i, group) in info.property_groups.iter().enumerate() {
            parquet_only(format!("{at}, property group {i}"), group.file_type)?;
        }
        Ok(Self {
            store,
            root: root.clone(),
            info,
            options,
            at,
            count: 0,
            chunk: 0,
            pending: Vec::new(),
            pending_rows: 0,
        })
    }

    /// The next vertices, in order.
    ///
    /// # Errors
    ///
    /// A column missing or of a type that does not hold its property, a null where none may be,
    /// or the store's refusal of a chunk.
    pub async fn write(&mut self, batch: &RecordBatch) -> Result<(), Error> {
        let mut columns = Vec::new();
        for group in &self.info.property_groups {
            columns.extend(group_columns(
                &self.at,
                batch,
                group,
                &self.options.user_types,
            )?);
        }
        if batch.num_rows() == 0 {
            return Ok(());
        }
        self.pending.push(batch_of(columns)?);
        self.pending_rows += batch.num_rows() as u64;
        self.count += batch.num_rows() as u64;
        while self.pending_rows >= self.info.chunk_size {
            self.flush(self.info.chunk_size).await?;
        }
        Ok(())
    }

    /// Writes the first `len` pending rows as the next chunk of every group.
    async fn flush(&mut self, len: u64) -> Result<(), Error> {
        let pending = one(&self.at, &self.pending)?;
        let begin = self.chunk * self.info.chunk_size;
        let ids: ArrayRef = Arc::new(Int64Array::from_iter_values(
            (begin..begin + len).map(|i| i as i64),
        ));
        let rows = pending.slice(0, len as usize);
        let mut column = 0;
        for group in &self.info.property_groups {
            let width = group.properties.len();
            let mut chunk = vec![(index_field(VERTEX_INDEX_COL), ids.clone())];
            let schema = rows.schema();
            chunk.extend(
                (column..column + width).map(|c| (schema.field(c).clone(), rows.column(c).clone())),
            );
            column += width;
            let path = paths::vertex_chunk(self.info, group, self.chunk);
            put_parquet(
                self.store,
                &self.root,
                &path,
                &batch_of(chunk)?,
                &self.options.props,
            )
            .await?;
        }
        let rest = pending.num_rows() as u64 - len;
        self.pending = if rest == 0 {
            Vec::new()
        } else {
            vec![pending.slice(len as usize, rest as usize)]
        };
        self.pending_rows = rest;
        self.chunk += 1;
        Ok(())
    }

    /// Writes the last chunk and the `vertex_count`, and returns the count.
    ///
    /// # Errors
    ///
    /// The store's refusal.
    pub async fn finish(mut self) -> Result<u64, Error> {
        if self.pending_rows > 0 {
            self.flush(self.pending_rows).await?;
        }
        put_count(
            self.store,
            &self.root,
            &paths::vertex_count(self.info),
            self.count,
        )
        .await?;
        Ok(self.count)
    }
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
///
/// It is [`EdgeWriter`] handed the edges sorted as the list orders them.
pub async fn write_edges(
    store: &dyn ObjectStore,
    root: &Path,
    info: &EdgeInfo,
    adj_list_type: AdjListType,
    vertex_count: u64,
    batches: &[RecordBatch],
    options: &Options,
) -> Result<u64, Error> {
    let mut writer = EdgeWriter::new(store, root, info, adj_list_type, vertex_count, options)?;
    let table = one(&writer.at, batches)?;
    // A stable sort, by the aligned end for an ordered list and by its part for an unordered
    // one. A key that is not an id column is left for the writer to refuse.
    let key = table
        .column_by_name(writer.key_name())
        .and_then(|k| k.as_any().downcast_ref::<Int64Array>().cloned());
    let sorted = match key {
        Some(key) if key.null_count() == 0 => {
            let mut rows: Vec<u64> = (0..table.num_rows() as u64).collect();
            if adj_list_type.ordered() {
                rows.sort_by_key(|&r| key.value(r as usize));
            } else {
                let size = writer.vertex_chunk_size as i64;
                rows.sort_by_key(|&r| key.value(r as usize).div_euclid(size));
            }
            take_record_batch(&table, &UInt64Array::from(rows))?
        }
        _ => table,
    };
    writer.write(&sorted).await?;
    writer.finish().await
}

/// **Writes one adjacency list from a stream of batches**, clustered: the edges arrive in the
/// order the list keeps — ascending by the aligned end for an ordered list, ascending by its part
/// for an unordered one — across every call, and a batch out of that order is refused. A chunk is
/// written as soon as it is full and a part as soon as an edge of a later part arrives, so the
/// writer holds at most one chunk of edges and one offset per vertex of one vertex chunk.
///
/// Which vertices an edge's end ranges over, what a batch holds, and what is written per part are
/// as for [`write_edges`]. [`finish`](Self::finish) closes the last part, writes a count and an
/// offset chunk for every part no edge reached, and the list's `vertex_count`.
///
/// This is the writer of a clustered input — Iceberg's `ClusteredWriter`, Spark GraphAr's
/// `EdgeWriter` after its repartition by vertex chunk: how the edges came to be in order (an
/// in-memory sort, or partitions sorted one at a time) is the caller's.
pub struct EdgeWriter<'a> {
    store: &'a dyn ObjectStore,
    root: Path,
    info: &'a EdgeInfo,
    adj: &'a AdjacentList,
    adj_list_type: AdjListType,
    options: &'a Options,
    at: String,
    vertex_count: u64,
    vertex_chunk_size: u64,
    /// The part being filled, its edges so far, and its next chunk.
    part: u64,
    part_edges: u64,
    chunk: u64,
    /// For an ordered list, the edges of each vertex of the part's vertex chunk.
    degrees: Vec<i64>,
    /// The last key handed, which the next may not precede.
    last: i64,
    /// Edges not yet in a chunk: the ends, then the groups' columns.
    pending: Vec<RecordBatch>,
    pending_rows: u64,
    edges: u64,
}

impl<'a> EdgeWriter<'a> {
    /// A writer for `info`'s list of type `adj_list_type` under `root`.
    ///
    /// # Errors
    ///
    /// An invalid `info`, an adjacency list it does not have, or a file in a type other than
    /// Parquet.
    pub fn new(
        store: &'a dyn ObjectStore,
        root: &Path,
        info: &'a EdgeInfo,
        adj_list_type: AdjListType,
        vertex_count: u64,
        options: &'a Options,
    ) -> Result<Self, Error> {
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
        let vertex_chunk_size = if adj_list_type.by_source() {
            info.src_chunk_size
        } else {
            info.dst_chunk_size
        };
        let mut writer = Self {
            store,
            root: root.clone(),
            info,
            adj,
            adj_list_type,
            options,
            at,
            vertex_count,
            vertex_chunk_size,
            part: 0,
            part_edges: 0,
            chunk: 0,
            degrees: Vec::new(),
            last: 0,
            pending: Vec::new(),
            pending_rows: 0,
            edges: 0,
        };
        writer.start_part();
        Ok(writer)
    }

    fn key_name(&self) -> &'static str {
        if self.adj_list_type.by_source() {
            SRC_INDEX_COL
        } else {
            DST_INDEX_COL
        }
    }

    fn start_part(&mut self) {
        let first = self.part * self.vertex_chunk_size;
        let last = (first + self.vertex_chunk_size).min(self.vertex_count);
        self.part_edges = 0;
        self.chunk = 0;
        self.degrees = if self.adj_list_type.ordered() {
            vec![0; last.saturating_sub(first) as usize]
        } else {
            Vec::new()
        };
    }

    /// The next edges, in the list's order.
    ///
    /// # Errors
    ///
    /// An end column missing, not `int64`, with a null, a negative id or one not below the vertex
    /// count; an edge out of the list's order; a property column as for [`write_edges`]; or the
    /// store's refusal of a chunk.
    pub async fn write(&mut self, batch: &RecordBatch) -> Result<(), Error> {
        let at = self.at.clone();
        let ends = |name: &str| -> Result<Int64Array, Error> {
            let array = column(&at, batch, name)?;
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
        let key = if self.adj_list_type.by_source() {
            &src
        } else {
            &dst
        };
        let vertex_count = self.vertex_count;
        if let Some(&beyond) = key.values().iter().find(|&&id| id as u64 >= vertex_count) {
            return Err(invalid(
                &at,
                format!("id {beyond} is not below the vertex count, {vertex_count}"),
            ));
        }
        let mut columns: Vec<(Field, ArrayRef)> = vec![
            (index_field(SRC_INDEX_COL), Arc::new(src.clone())),
            (index_field(DST_INDEX_COL), Arc::new(dst.clone())),
        ];
        for group in &self.info.property_groups {
            columns.extend(group_columns(&at, batch, group, &self.options.user_types)?);
        }
        let rows = batch_of(columns)?;
        let size = self.vertex_chunk_size;
        let ordered = self.adj_list_type.ordered();
        let keys = key.values();
        let mut begin = 0;
        while begin < keys.len() {
            let part = keys[begin] as u64 / size;
            let end = begin
                + keys[begin..]
                    .iter()
                    .take_while(|&&k| k as u64 / size == part)
                    .count();
            for &k in &keys[begin..end] {
                let out_of_order = if ordered {
                    k < self.last
                } else {
                    (k as u64 / size) < self.part
                };
                if out_of_order {
                    return Err(invalid(
                        &at,
                        format!(
                            "id {k} arrives after {}, out of the list's order",
                            self.last
                        ),
                    ));
                }
                self.last = k;
            }
            if part < self.part {
                return Err(invalid(
                    &at,
                    format!("part {part} arrives after part {}", self.part),
                ));
            }
            self.advance_to(part).await?;
            if ordered {
                let first = (part * size) as i64;
                for &k in &keys[begin..end] {
                    self.degrees[(k - first) as usize] += 1;
                }
            }
            let n = (end - begin) as u64;
            self.pending.push(rows.slice(begin, end - begin));
            self.pending_rows += n;
            self.part_edges += n;
            self.edges += n;
            while self.pending_rows >= self.info.chunk_size {
                self.flush(self.info.chunk_size).await?;
            }
            begin = end;
        }
        Ok(())
    }

    /// Closes parts until `part` is the one being filled.
    async fn advance_to(&mut self, part: u64) -> Result<(), Error> {
        while self.part < part {
            self.close_part().await?;
            self.part += 1;
            self.start_part();
        }
        Ok(())
    }

    /// The part's last chunk, its count, and for an ordered list its offset chunk.
    async fn close_part(&mut self) -> Result<(), Error> {
        if self.pending_rows > 0 {
            self.flush(self.pending_rows).await?;
        }
        put_count(
            self.store,
            &self.root,
            &paths::edge_count(self.info, self.adj, self.part),
            self.part_edges,
        )
        .await?;
        if self.adj_list_type.ordered() {
            // C++ `EdgesBuilder::getOffsetTable`: for each vertex of the chunk, the number of the
            // part's edges whose end is at or before it.
            let mut seen = 0;
            let offsets = Int64Array::from_iter_values(std::iter::once(0).chain(
                self.degrees.iter().map(|d| {
                    seen += d;
                    seen
                }),
            ));
            let batch = batch_of(vec![(index_field(OFFSET_COL), Arc::new(offsets))])?;
            put_parquet(
                self.store,
                &self.root,
                &paths::offset_chunk(self.info, self.adj, self.part),
                &batch,
                &self.options.props,
            )
            .await?;
        }
        Ok(())
    }

    /// Writes the first `len` pending edges as the part's next chunk.
    async fn flush(&mut self, len: u64) -> Result<(), Error> {
        let pending = one(&self.at, &self.pending)?;
        let rows = pending.slice(0, len as usize);
        put_parquet(
            self.store,
            &self.root,
            &paths::adj_list_chunk(self.info, self.adj, self.part, self.chunk),
            &rows.project(&[0, 1])?,
            &self.options.props,
        )
        .await?;
        let mut column = 2;
        for group in &self.info.property_groups {
            let width = group.properties.len();
            let projection: Vec<usize> = (column..column + width).collect();
            column += width;
            let path =
                paths::edge_property_chunk(self.info, self.adj, group, self.part, self.chunk);
            put_parquet(
                self.store,
                &self.root,
                &path,
                &rows.project(&projection)?,
                &self.options.props,
            )
            .await?;
        }
        let rest = pending.num_rows() as u64 - len;
        self.pending = if rest == 0 {
            Vec::new()
        } else {
            vec![pending.slice(len as usize, rest as usize)]
        };
        self.pending_rows = rest;
        self.chunk += 1;
        Ok(())
    }

    /// Closes the last part, writes a count and an offset chunk for every part no edge reached,
    /// then the list's `vertex_count`, and returns the number of edges.
    ///
    /// # Errors
    ///
    /// The store's refusal.
    pub async fn finish(mut self) -> Result<u64, Error> {
        let parts = chunks(self.vertex_count, self.vertex_chunk_size);
        self.advance_to(parts).await?;
        put_count(
            self.store,
            &self.root,
            &paths::edge_vertex_count(self.info, self.adj),
            self.vertex_count,
        )
        .await?;
        Ok(self.edges)
    }
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
