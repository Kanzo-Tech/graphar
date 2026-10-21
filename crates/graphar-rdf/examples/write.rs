//! Writes an RDF graph and its shapes as GraphAr to a directory:
//!
//! ```sh
//! cargo run -p graphar-rdf --example write -- <shapes.ttl | shapes.shex | shapes.shex.json> <data.ttl | data.nt> <dir> [chunk_size]
//! ```
//!
//! Each subject is typed by the shape it validates against. A graph that does not conform to the
//! shapes is refused, naming the subject or the triple and why.

use futures::executor::block_on;
use graphar_rdf::{Options, Typing, shapes, write};
use object_store::{local::LocalFileSystem, path::Path};
use oxttl::{NTriplesParser, TurtleParser};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [shapes_file, data, dir, rest @ ..] = args.as_slice() else {
        return Err("usage: write <shapes> <data> <dir> [chunk_size]".into());
    };
    let shapes = match shapes_file.as_str() {
        f if f.ends_with(".json") => shapes::shex::from_json(&std::fs::read_to_string(f)?)?,
        f if f.ends_with(".shex") => shapes::shex::from_compact(&std::fs::read_to_string(f)?)?,
        f => shapes::shacl::from_turtle(&std::fs::read_to_string(f)?)?,
    };
    let bytes = std::fs::read(data)?;
    let triples: Vec<oxrdf::Triple> = if data.ends_with(".nt") {
        NTriplesParser::new()
            .for_slice(&bytes)
            .collect::<Result<_, _>>()?
    } else {
        TurtleParser::new()
            .for_slice(&bytes)
            .collect::<Result<_, _>>()?
    };
    std::fs::create_dir_all(dir)?;
    let store = LocalFileSystem::new_with_prefix(dir)?;
    let options = Options {
        chunk_size: rest
            .first()
            .map(|c| c.parse())
            .transpose()?
            .unwrap_or(1 << 20),
        ..Options::default()
    };
    let written = block_on(write(
        &store,
        &Path::from(""),
        &Typing::Validate(&shapes),
        triples,
        &options,
    ))?;
    println!(
        "{} triples: {:?}, {:?}",
        written.triples, written.vertices, written.edges
    );
    Ok(())
}
