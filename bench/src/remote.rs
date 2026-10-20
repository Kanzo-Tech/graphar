//! Access over object storage: what a reader with nothing but the bucket asks of it, counted.
//!
//! Every layout is read through [`Counting`], an `object_store` that counts each GET (whole or a
//! byte range) and HEAD, the bytes the GETs return, and each LIST: one call is one request an S3
//! bucket would serve, since `object_store`'s S3 client issues exactly one HTTP request per call
//! and coalesces ranges as the default `get_ranges` here does. Each access starts cold (a new
//! engine, nothing cached), so the count is what a serverless reader pays for that answer alone.
//!
//! GAR and SPO are read by DataFusion, with filter pushdown and the page index, the tables a query
//! names registered from their chunks; Zarr by its layout's own reader through `zarrs`, counted at
//! its storage interface the same way (a whole value or a byte range, one GET each); HDT and
//! N-Triples have no partial access, and are one GET of the whole file.

use std::fmt;
use std::ops::Range;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use datafusion::arrow::array::{Array as _, Int64Array};
use datafusion::prelude::{ParquetReadOptions, SessionConfig, SessionContext};
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

/// The tables of a Parquet layout: each name and the directory (or file) under the layout's root
/// that holds it.
pub type Tables = Vec<(String, String)>;

/// The answer of `sql` over `tables` under `root`, read cold through a counting store, and what it
/// cost.
pub async fn count_sql(root: &Path, tables: &Tables, sql: &str) -> (u64, Value) {
    let store = Arc::new(Counting::local(root));
    let counts = store.counts.clone();
    let mut config = SessionConfig::new();
    config.options_mut().execution.parquet.pushdown_filters = true;
    config.options_mut().execution.parquet.reorder_filters = true;
    // An adjacency list's chunks are under `part<n>/`.
    config
        .options_mut()
        .execution
        .listing_table_ignore_subdirectory = false;
    let ctx = SessionContext::new_with_config(config);
    let url = url::Url::parse("bench://layout/").expect("a URL");
    ctx.register_object_store(&url, store);
    for (name, path) in tables {
        // Only what the query names: a reader opens no table it does not read.
        if !sql.contains(&format!("\"{name}\"")) && !sql.contains(&format!(" {name} ")) {
            continue;
        }
        ctx.register_parquet(
            TableReference::bare(name.clone()),
            format!("bench://layout/{path}"),
            // GraphAr's chunks have no extension.
            ParquetReadOptions {
                file_extension: "",
                ..ParquetReadOptions::default()
            },
        )
        .await
        .unwrap_or_else(|e| panic!("{name}: {e}"));
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
