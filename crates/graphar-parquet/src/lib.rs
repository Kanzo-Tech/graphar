//! Apache GraphAr's data files in Parquet: the vertex property chunks, the adjacency-list,
//! offset and edge property chunks, and the count files, written and read through any
//! [`object_store::ObjectStore`].
//!
//! The reference is the [GraphAr format specification], and where it is silent — the file names,
//! the count files and the reserved column names are not in it — the C++ library
//! (`apache/incubator-graphar` v0.13.0): `graph_info.cc` for the paths, `general_params.h` for the
//! columns, `arrow/chunk_writer.cc` and `high-level/edges_builder.cc` for what a writer puts in
//! each file. Where this crate departs from C++, the item says so.
//!
//! Every function takes the store and the graph's **root**: the directory its data files are
//! under, which is GraphAr's default — the directory of the graph document — when the graph
//! names no `prefix`. The paths under it are [`paths`]' and come from the information files of
//! [`graphar_info`] alone.
//!
//! **No runtime.** Parquet's `async` and `object_store` features both bring `tokio`, so a chunk
//! is encoded in memory with the synchronous writer and put whole, and read whole before it is
//! decoded. A chunk is at most `chunk_size` rows by construction, which is what bounds it. The
//! crate builds for `wasm32-unknown-unknown`, and its normal dependencies reach no `tokio`.
//!
//! **Compression is the caller's.** [`WriterProperties`](parquet::file::properties::WriterProperties)
//! names the codec, and a codec is a feature of `parquet` that the caller turns on.
//!
//! [GraphAr format specification]: https://graphar.apache.org/docs/specification/format

mod error;
pub mod paths;
mod read;
mod types;
mod write;

pub use types::{Options, UserTypes};

pub use error::Error;
pub use read::{
    read_adj_list_chunk, read_count, read_edge_property_chunk, read_offsets, read_vertex_chunk,
};
pub use write::{write_edges, write_info, write_vertices};

/// The internal vertex id in every vertex chunk: `int64`, the row of the vertex in its type's
/// logical table. C++ `GeneralParams::kVertexIndexCol`.
pub const VERTEX_INDEX_COL: &str = "_graphArVertexIndex";
/// The source's internal vertex id in an adjacency-list chunk. C++ `GeneralParams::kSrcIndexCol`.
pub const SRC_INDEX_COL: &str = "_graphArSrcIndex";
/// The destination's internal vertex id in an adjacency-list chunk. C++
/// `GeneralParams::kDstIndexCol`.
pub const DST_INDEX_COL: &str = "_graphArDstIndex";
/// The one column of an offset chunk. C++ `GeneralParams::kOffsetCol`.
pub const OFFSET_COL: &str = "_graphArOffset";
