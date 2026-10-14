//! Writes the graphs of the tests under a directory: `conformance/sample/`, which GraphAr's own
//! C++ reader and `graphar-duckdb` open, and `conformance/user_defined/`, which C++ refuses and
//! `graphar-duckdb` opens.
//!
//! ```sh
//! cargo run -p graphar-parquet --example sample -- conformance
//! ```

#[path = "../tests/sample/mod.rs"]
mod sample;

use object_store::{local::LocalFileSystem, path::Path};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = std::env::args().nth(1).ok_or("usage: sample <directory>")?;
    for (name, write) in [
        ("sample", Graph::Sample),
        ("user_defined", Graph::UserDefined),
    ] {
        let dir = format!("{dir}/{name}");
        std::fs::create_dir_all(&dir)?;
        let store = LocalFileSystem::new_with_prefix(&dir)?;
        futures::executor::block_on(async {
            match write {
                Graph::Sample => sample::write(&store, &Path::default()).await,
                Graph::UserDefined => sample::write_user_defined(&store, &Path::default()).await,
            }
        })?;
    }
    Ok(())
}

enum Graph {
    Sample,
    UserDefined,
}
