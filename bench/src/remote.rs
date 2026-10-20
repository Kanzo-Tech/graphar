//! Access over object storage: what a reader with nothing but the bucket asks of it, counted.
//!
//! Every layout is read through [`Counting`], an `object_store` that counts each GET (whole or a
//! byte range) and HEAD, the bytes the GETs return, and each LIST: one call is one request an S3
//! bucket would serve, since `object_store`'s S3 client issues exactly one HTTP request per call
//! and coalesces ranges as the default `get_ranges` here does. Each access starts cold (a new
//! engine, nothing cached), so the count is what a serverless reader pays for that answer alone.
//!
//! GAR and SPO are read by DataFusion, with filter pushdown and the page index, the tables a query
//! names registered from their files by name: GAR's as the graph document, the table's information
//! file and its count files name them (each a GET), so no layout is listed; Zarr by its layout's own reader through `zarrs`, counted at
//! its storage interface the same way (a whole value or a byte range, one GET each); HDT and
//! N-Triples have no partial access, and are one GET of the whole file.

use std::fmt;
use std::ops::Range;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use datafusion::arrow::array::{Array as _, Int64Array};
use datafusion::prelude::{SessionConfig, SessionContext};
use datafusion::sql::TableReference;
use futures::stream::BoxStream;
use object_store::local::LocalFileSystem;
use object_store::path::Path as StorePath;
use object_store::{
    CopyOptions, GetOptions, GetResult, ListResult, MultipartUpload, ObjectMeta, ObjectStore,
    PutMultipartOptions, PutOptions, PutPayload, PutResult, Result,
};
use serde_json::{Value, json};
use zarrs::filesystem::FilesystemStore;
use zarrs::storage::byte_range::{ByteRange, ByteRangeIterator};
use zarrs::storage::{MaybeBytesIterator, ReadableStorageTraits, StorageError, StoreKey};

/// Requests and bytes, as a bucket would bill them.
#[derive(Debug, Default)]
pub struct Counts {
    gets: AtomicU64,
    heads: AtomicU64,
    lists: AtomicU64,
    bytes: AtomicU64,
}

impl Counts {
    pub fn json(&self) -> Value {
        let gets = self.gets.load(Ordering::Relaxed);
        let heads = self.heads.load(Ordering::Relaxed);
        let lists = self.lists.load(Ordering::Relaxed);
        json!({
            "requests": gets + heads + lists,
            "gets": gets,
            "heads": heads,
            "lists": lists,
            "bytes": self.bytes.load(Ordering::Relaxed),
        })
    }
}

/// An `object_store` that counts what is asked of the one it wraps.
#[derive(Debug)]
pub struct Counting {
    inner: Arc<dyn ObjectStore>,
    pub counts: Arc<Counts>,
}

impl Counting {
    pub fn local(dir: &Path) -> Self {
        Self {
            inner: Arc::new(LocalFileSystem::new_with_prefix(dir).expect("a local store")),
            counts: Arc::default(),
        }
    }
}

impl fmt::Display for Counting {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Counting({})", self.inner)
    }
}

#[async_trait::async_trait]
impl ObjectStore for Counting {
    async fn put_opts(
        &self,
        location: &StorePath,
        payload: PutPayload,
        opts: PutOptions,
    ) -> Result<PutResult> {
        self.inner.put_opts(location, payload, opts).await
    }

    async fn put_multipart_opts(
        &self,
        location: &StorePath,
        opts: PutMultipartOptions,
    ) -> Result<Box<dyn MultipartUpload>> {
        self.inner.put_multipart_opts(location, opts).await
    }

    async fn get_opts(&self, location: &StorePath, options: GetOptions) -> Result<GetResult> {
        let head = options.head;
        let result = self.inner.get_opts(location, options).await?;
        if head {
            self.counts.heads.fetch_add(1, Ordering::Relaxed);
        } else {
            self.counts.gets.fetch_add(1, Ordering::Relaxed);
            let Range { start, end } = result.range;
            self.counts.bytes.fetch_add(end - start, Ordering::Relaxed);
        }
        Ok(result)
    }

    fn delete_stream(
        &self,
        locations: BoxStream<'static, Result<StorePath>>,
    ) -> BoxStream<'static, Result<StorePath>> {
        self.inner.delete_stream(locations)
    }

    fn list(&self, prefix: Option<&StorePath>) -> BoxStream<'static, Result<ObjectMeta>> {
        self.counts.lists.fetch_add(1, Ordering::Relaxed);
        self.inner.list(prefix)
    }

    async fn list_with_delimiter(&self, prefix: Option<&StorePath>) -> Result<ListResult> {
        self.counts.lists.fetch_add(1, Ordering::Relaxed);
        self.inner.list_with_delimiter(prefix).await
    }

    async fn copy_opts(
        &self,
        from: &StorePath,
        to: &StorePath,
        options: CopyOptions,
    ) -> Result<()> {
        self.inner.copy_opts(from, to, options).await
    }
}

/// Where a Parquet layout's tables are.
pub enum Layout<'a> {
    /// Each table, one file.
    Files(Vec<(String, String)>),
    /// A GraphAr graph: each table's chunks are named by the information files and the count
    /// files, so a reader lists nothing.
    Gar(&'a graphar_info::GraphInfo),
}

/// The chunks of GraphAr table `name`, as a reader that has read the graph document finds them
/// with nothing but the store: the table's information file, then its count files, a GET each.
async fn gar_chunks(store: &Counting, info: &graphar_info::GraphInfo, name: &str) -> Vec<String> {
    use graphar_parquet::{paths, read_count};
    let root = StorePath::from("");
    let get = |file: String| async move {
        store
            .get_opts(&StorePath::from(file.as_str()), GetOptions::default())
            .await
            .unwrap_or_else(|e| panic!("{file}: {e}"))
            .bytes()
            .await
            .expect("bytes")
    };
    if let Some(v) = info.vertices.iter().find(|v| v.r#type == name) {
        get(format!("{name}.vertex.yml")).await;
        let n = read_count(store, &root, &paths::vertex_count(v))
            .await
            .expect("vertex_count");
        let group = &v.property_groups[0];
        return (0..n.div_ceil(v.chunk_size))
            .map(|k| paths::vertex_chunk(v, group, k))
            .collect();
    }
    let e = info
        .edges
        .iter()
        .find(|e| format!("{}_{}_{}", e.src_type, e.edge_type, e.dst_type) == name)
        .unwrap_or_else(|| panic!("no table {name}"));
    get(format!("{name}.edge.yml")).await;
    let adj = &e.adj_lists[0];
    let sources = read_count(store, &root, &paths::edge_vertex_count(e, adj))
        .await
        .expect("vertex_count");
    let mut out = Vec::new();
    for part in 0..sources.div_ceil(e.src_chunk_size) {
        let edges = read_count(store, &root, &paths::edge_count(e, adj, part))
            .await
            .expect("edge_count");
        out.extend(
            (0..edges.div_ceil(e.chunk_size)).map(|k| paths::adj_list_chunk(e, adj, part, k)),
        );
    }
    out
}

/// The answer of `sql` over the tables of `layout` under `root`, read cold through a counting
/// store, and what it cost. A table is registered from its files by name, never by listing a
/// directory, and only if the query names it.
pub async fn count_sql(root: &Path, layout: &Layout<'_>, sql: &str) -> (u64, Value) {
    use datafusion::datasource::file_format::parquet::ParquetFormat;
    use datafusion::datasource::listing::{
        ListingOptions, ListingTable, ListingTableConfig, ListingTableUrl,
    };
    let store = Arc::new(Counting::local(root));
    let counts = store.counts.clone();
    let mut config = SessionConfig::new();
    config.options_mut().execution.parquet.pushdown_filters = true;
    config.options_mut().execution.parquet.reorder_filters = true;
    let ctx = SessionContext::new_with_config(config);
    let url = url::Url::parse("bench://layout/").expect("a URL");
    ctx.register_object_store(&url, store.clone());
    let names: Vec<String> = match layout {
        Layout::Files(files) => files.iter().map(|(n, _)| n.clone()).collect(),
        Layout::Gar(info) => info
            .vertices
            .iter()
            .map(|v| v.r#type.clone())
            .chain(
                info.edges
                    .iter()
                    .map(|e| format!("{}_{}_{}", e.src_type, e.edge_type, e.dst_type)),
            )
            .collect(),
    };
    if let Layout::Gar(info) = layout {
        // The graph document, once: it names every information file.
        let document = StorePath::from(format!("{}.graph.yml", info.name).as_str());
        store
            .get_opts(&document, GetOptions::default())
            .await
            .expect("the graph document")
            .bytes()
            .await
            .expect("bytes");
    }
    for name in names {
        // Only what the query names: a reader opens no table it does not read.
        if !sql.contains(&format!("\"{name}\"")) && !sql.contains(&format!(" {name} ")) {
            continue;
        }
        let files = match layout {
            Layout::Files(files) => vec![
                files
                    .iter()
                    .find(|(n, _)| *n == name)
                    .expect("a table")
                    .1
                    .clone(),
            ],
            Layout::Gar(info) => gar_chunks(&store, info, &name).await,
        };
        let urls = files
            .iter()
            .map(|f| ListingTableUrl::parse(format!("bench://layout/{f}")).expect("a URL"))
            .collect();
        // GraphAr's chunks have no extension.
        let options =
            ListingOptions::new(Arc::new(ParquetFormat::default())).with_file_extension("");
        let table_config = ListingTableConfig::new_with_multi_paths(urls)
            .with_listing_options(options)
            .infer_schema(&ctx.state())
            .await
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        let table = ListingTable::try_new(table_config).expect("a table");
        ctx.register_table(TableReference::bare(name.clone()), Arc::new(table))
            .expect("registered");
    }
    let batches = ctx
        .sql(sql)
        .await
        .unwrap_or_else(|e| panic!("{sql}: {e}"))
        .collect()
        .await
        .unwrap_or_else(|e| panic!("{sql}: {e}"));
    let column = batches[0]
        .column(0)
        .as_any()
        .downcast_ref::<Int64Array>()
        .expect("a count");
    assert!(!column.is_null(0));
    (column.value(0) as u64, counts.json())
}

/// Reading a whole file: one GET of all of it.
pub fn whole(path: &Path) -> Value {
    let bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    json!({"requests": 1, "gets": 1, "heads": 0, "lists": 0, "bytes": bytes})
}

/// The Zarr access through a counting store, cold: the group (and its dictionaries), the three
/// arrays' metadata, the chunk.
pub fn count_zarr(dir: &Path, pattern: &crate::zarr::Pattern<'_>) -> (u64, Value) {
    let store = Arc::new(CountingZarr {
        inner: FilesystemStore::new(dir).expect("a Zarr store"),
        counts: Arc::default(),
    });
    let n = crate::zarr::Zarr::open(store.clone()).count(pattern);
    (n, store.counts.json())
}

/// A Zarr store that counts what is asked of the one it wraps: a whole value is one GET, each
/// byte range another.
struct CountingZarr {
    inner: FilesystemStore,
    counts: Arc<Counts>,
}

impl ReadableStorageTraits for CountingZarr {
    fn get_partial_many<'a>(
        &'a self,
        key: &StoreKey,
        byte_ranges: ByteRangeIterator<'a>,
    ) -> std::result::Result<MaybeBytesIterator<'a>, StorageError> {
        let ranges: Vec<ByteRange> = byte_ranges.collect();
        let Some(bytes) = self
            .inner
            .get_partial_many(key, Box::new(ranges.clone().into_iter()))?
        else {
            return Ok(None);
        };
        let bytes: Vec<_> = bytes.collect::<std::result::Result<_, _>>()?;
        self.counts
            .gets
            .fetch_add(ranges.len() as u64, Ordering::Relaxed);
        let n: usize = bytes.iter().map(|b: &zarrs::storage::Bytes| b.len()).sum();
        self.counts.bytes.fetch_add(n as u64, Ordering::Relaxed);
        Ok(Some(Box::new(bytes.into_iter().map(Ok))))
    }

    fn size_key(&self, key: &StoreKey) -> std::result::Result<Option<u64>, StorageError> {
        self.counts.heads.fetch_add(1, Ordering::Relaxed);
        self.inner.size_key(key)
    }

    fn supports_get_partial(&self) -> bool {
        self.inner.supports_get_partial()
    }
}
