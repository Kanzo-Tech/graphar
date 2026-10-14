//! Writes the sample graph of the tests to a directory: `conformance/sample/`, which GraphAr's own
//! C++ reader and `graphar-duckdb` open.
//!
//! ```sh
//! cargo run -p graphar-parquet --example sample -- conformance/sample
//! ```

#[path = "../tests/sample/mod.rs"]
mod sample;

use object_store::{local::LocalFileSystem, path::Path};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = std::env::args().nth(1).ok_or("usage: sample <directory>")?;
    std::fs::create_dir_all(&dir)?;
    let store = LocalFileSystem::new_with_prefix(&dir)?;
    futures::executor::block_on(sample::write(&store, &Path::default()))?;
    Ok(())
}
