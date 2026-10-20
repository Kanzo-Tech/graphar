//! The same triples written six ways, each timed and measured on disk.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use arrow_array::{ArrayRef, RecordBatch, StringArray};
use futures::executor::block_on;
use graphar_rdf::{Options, Schema, shapes, write};
use object_store::local::LocalFileSystem;
use oxrdf::{NamedOrBlankNode, Term, Triple};
use oxttl::NTriplesSerializer;
use parquet::arrow::ArrowWriter;
use parquet::basic::{Compression, ZstdLevel};
use parquet::file::properties::WriterProperties;
use serde_json::{Value, json};

use crate::{generate, timed};

/// What [`write_all`] wrote, and where.
pub struct Written {
    pub triples: usize,
    pub schema: Schema,
    pub nt: PathBuf,
    pub hdt: PathBuf,
    pub spo: PathBuf,
    pub gar: PathBuf,
    pub zarr: PathBuf,
    pub gar_summary: Value,
    pub sizes: Value,
    pub write_seconds: Value,
    pub stats_share: f64,
}

/// The rows of a GAR chunk: 2^16, so that the largest tables of the larger scales are several
/// chunks and their Puffin files are written and counted.
pub const CHUNK_SIZE: u64 = 1 << 16;

/// Parquet's settings for GAR and SPO alike: zstd, and the writer's default row groups.
pub fn props() -> WriterProperties {
    WriterProperties::builder()
        .set_compression(Compression::ZSTD(ZstdLevel::default()))
        .build()
}

fn size(path: &Path) -> u64 {
    if path.is_dir() {
        std::fs::read_dir(path)
            .expect("a directory")
            .map(|e| size(&e.expect("an entry").path()))
            .sum()
    } else {
        std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
    }
}

/// The number of files under `path`: a request each, to read them all from a bucket.
fn files(path: &Path) -> u64 {
    if path.is_dir() {
        std::fs::read_dir(path)
            .expect("a directory")
            .map(|e| files(&e.expect("an entry").path()))
            .sum()
    } else {
        u64::from(path.exists())
    }
}

/// The S/P/O row of a triple: subject, predicate and object as text, the object's datatype and
/// language; a blank node is `_:` and its label.
fn spo_row(t: &Triple) -> [Option<String>; 5] {
    let subject = match &t.subject {
        NamedOrBlankNode::NamedNode(n) => n.as_str().to_owned(),
        NamedOrBlankNode::BlankNode(b) => format!("_:{}", b.as_str()),
    };
    let (object, datatype, lang) = match &t.object {
        Term::NamedNode(n) => (n.as_str().to_owned(), None, None),
        Term::BlankNode(b) => (format!("_:{}", b.as_str()), None, None),
        Term::Literal(l) => (
            l.value().to_owned(),
            Some(l.datatype().as_str().to_owned()),
            l.language().map(str::to_owned),
        ),
    };
    [
        Some(subject),
        Some(t.predicate.as_str().to_owned()),
        Some(object),
        datatype,
        lang,
    ]
}

fn write_spo(path: &Path, triples: &[Triple]) {
    let mut rows: Vec<[Option<String>; 5]> = triples.iter().map(spo_row).collect();
    rows.sort();
    rows.dedup();
    let names = ["subject", "predicate", "object", "datatype", "lang"];
    let batch = RecordBatch::try_from_iter(names.iter().enumerate().map(|(i, n)| {
        let column: ArrayRef = Arc::new(
            rows.iter()
                .map(|r| r[i].as_deref())
                .collect::<StringArray>(),
        );
        (*n, column)
    }))
    .expect("a batch");
    let file = File::create(path).expect("the SPO file");
    let mut writer = ArrowWriter::try_new(file, batch.schema(), Some(props())).expect("a writer");
    writer.write(&batch).expect("written");
    writer.close().expect("closed");
}

fn write_gar(dir: &Path, schema: &Schema, triples: &[Triple], stats: bool) -> graphar_rdf::Written {
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir).expect("the GAR directory");
    let store = LocalFileSystem::new_with_prefix(dir).expect("a store");
    let options = Options {
        name: "lubm".to_owned(),
        chunk_size: CHUNK_SIZE,
        props: props(),
        stats,
    };
    block_on(write(
        &store,
        &object_store::path::Path::from(""),
        schema,
        triples.to_vec(),
        &options,
    ))
    .expect("GAR written")
}

/// Writes `triples` as N-Triples, gzipped N-Triples, HDT, SPO, GAR and Zarr under `dir`.
pub fn write_all(dir: &Path, triples: &[Triple], runs: usize) -> Written {
    let schema = shapes::shacl::from_turtle(generate::SHAPES).expect("the shapes");
    let nt = dir.join("data.nt");
    let mut out =
        NTriplesSerializer::new().for_writer(BufWriter::new(File::create(&nt).expect("NT")));
    for t in triples {
        out.serialize_triple(t).expect("a triple");
    }
    out.finish().into_inner().expect("flushed");

    let nt_gz = dir.join("data.nt.gz");
    let (gz_seconds, ()) = timed(runs, || {
        let mut gz = flate2::write::GzEncoder::new(
            BufWriter::new(File::create(&nt_gz).expect("NT.gz")),
            flate2::Compression::default(),
        );
        gz.write_all(&std::fs::read(&nt).expect("NT"))
            .expect("gzipped");
        gz.finish().expect("finished").flush().expect("flushed");
    });

    let hdt = dir.join("data.hdt");
    let (hdt_seconds, ()) = timed(runs, || {
        let h = hdt::Hdt::read_nt(&nt).expect("HDT from N-Triples");
        let mut w = BufWriter::new(File::create(&hdt).expect("HDT"));
        h.write(&mut w).expect("HDT written");
        w.flush().expect("flushed");
    });

    let spo = dir.join("spo.parquet");
    let (spo_seconds, ()) = timed(runs, || write_spo(&spo, triples));

    let gar = dir.join("gar");
    let (gar_bare_seconds, _) = timed(runs, || write_gar(&gar, &schema, triples, false));
    let (gar_seconds, summary) = timed(runs, || write_gar(&gar, &schema, triples, true));

    let zarr = dir.join("zarr");
    let (zarr_seconds, ()) = timed(runs, || crate::zarr::write(&zarr, triples));

    let sizes = json!({
        "nt": size(&nt),
        "nt_gz": size(&nt_gz),
        "hdt": size(&hdt),
        "spo": size(&spo),
        "gar": size(&gar),
        "zarr": size(&zarr),
        "zarr_files": files(&zarr),
        "gar_files": files(&gar),
        "gar_mapping_ttl": size(&gar.join(graphar_rdf::MAPPING_FILE)),
        "gar_puffin": summary.info.vertices.iter().map(|v| size(&gar.join(graphar_stats::puffin_path(v)))).sum::<u64>(),
    });
    Written {
        triples: summary.triples as usize,
        gar_summary: json!({
            "vertices": summary.vertices.iter().map(|(l, n)| json!({"type": l, "rows": n})).collect::<Vec<_>>(),
            "edges": summary.edges.iter().map(|(l, n)| json!({"type": l, "edges": n})).collect::<Vec<_>>(),
        }),
        schema,
        nt,
        hdt,
        spo,
        gar,
        zarr,
        sizes,
        write_seconds: json!({
            "nt_gz": gz_seconds,
            "hdt": hdt_seconds,
            "spo": spo_seconds,
            "gar": gar_seconds,
            "gar_without_stats": gar_bare_seconds,
            "zarr": zarr_seconds,
        }),
        stats_share: ((gar_seconds - gar_bare_seconds) / gar_seconds).max(0.0),
    }
}
